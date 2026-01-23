//! `#[trigger]` macro implementation.

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Pat, Type, parse_macro_input};

use crate::common::has_allow_sync;

/// Implementation of the `#[trigger]` attribute macro.
pub fn trigger_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;

    // Parse attributes
    let attr_str = attr.to_string();
    let template =
        parse_trigger_attr(&attr_str, "template").unwrap_or_else(|| "custom".to_string());

    // Parse target as a type identifier (not a string)
    let target_str =
        parse_trigger_attr(&attr_str, "target").unwrap_or_else(|| "Unknown".to_string());
    let target_type: proc_macro2::TokenStream =
        target_str.parse().unwrap_or_else(|_| quote!(Unknown));

    // Extract the first parameter type (payload type)
    let payload_type: Option<Box<Type>> = sig.inputs.iter().find_map(|arg| {
        if let FnArg::Typed(pat_type) = arg {
            Some(pat_type.ty.clone())
        } else {
            None
        }
    });

    let payload_type_token = payload_type
        .clone()
        .unwrap_or_else(|| Box::new(syn::parse_quote!(())));
    let payload_type_str = quote!(#payload_type_token).to_string().replace(" ", "");

    // Compute expected target type based on template
    let _target_type_str = match template.as_str() {
        "custom" => "tokio::sync::Notify".to_string(),
        "queue" => format!(
            "tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{}>>",
            payload_type_str
        ),
        "cron" => "String".to_string(),
        _ => "unknown".to_string(),
    };

    // Extract additional DI parameters (index 2+)
    let mut di_resolve_tokens = Vec::new();
    let mut di_call_args = Vec::new();
    let mut param_entries = Vec::new();

    for (i, arg) in sig.inputs.iter().enumerate() {
        // Skip first two params (payload and trigger_id)
        if i < 2 {
            continue;
        }

        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let arg_name = &pat_ident.ident;
                let arg_type = &pat_type.ty;
                let arg_name_str = arg_name.to_string();
                let arg_type_str = quote!(#arg_type).to_string().replace(" ", "");

                // Add to param entries for verification with pre-computed key
                let key_str = format!("{}_{}", arg_name_str, arg_type_str);
                param_entries.push(quote! {
                    service_daemon::ServiceParam {
                        name: #arg_name_str,
                        type_name: #arg_type_str,
                        key: #key_str,
                    }
                });

                // Check if the type is Arc<T>
                if let Type::Path(type_path) = &**arg_type {
                    if let Some(segment) = type_path.path.segments.last() {
                        if segment.ident == "Arc" {
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                if let Some(syn::GenericArgument::Type(inner_type)) =
                                    args.args.first()
                                {
                                    // Type-Based DI: use T::resolve().await for async resolution
                                    di_resolve_tokens.push(quote! {
                                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                                    });
                                    di_call_args.push(quote! { #arg_name });
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Non-Arc types are not supported for DI
                abort!(
                    arg_type,
                    "Trigger DI parameters must be Arc<T> where T implements Provided";
                    help = "Wrap your type in Arc<T>, e.g., `Arc<MyType>` instead of `MyType`";
                    note = "Parameters at index 0 (payload) and 1 (trigger_id) are not injected"
                );
            }
        }
    }

    let wrapper_name = format_ident!("{}_trigger_wrapper", fn_name);
    let entry_name = format_ident!("__TRIGGER_ENTRY_{}", fn_name.to_string().to_uppercase());

    let is_async = input.sig.asyncness.is_some();
    let allow_sync_present = has_allow_sync(attrs);

    // Generate template-specific event loop
    let event_loop = match template.as_str() {
        // Signal/Notify triggers - aliases: custom, notify, event
        "custom" | "notify" | "event" => {
            let call_expr = if is_async {
                quote! { #fn_name((), trigger_id, #(#di_call_args.clone()),*).await }
            } else if allow_sync_present {
                quote! { #fn_name((), trigger_id, #(#di_call_args.clone()),*) }
            } else {
                quote! {
                    {
                        tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                        #fn_name((), trigger_id, #(#di_call_args.clone()),*)
                    }
                }
            };
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Signal trigger - resolve target using Type-Based DI (async)
                let notifier_wrapper = <#target_type as service_daemon::Provided>::resolve().await;

                loop {
                    notifier_wrapper.notified().await;
                    let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                    tracing::info!("Signal trigger '{}' fired (ID: {})", #fn_name_str, trigger_id);
                    if let Err(e) = #call_expr {
                        tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                    }
                }
            }
        }
        "queue" => {
            let call_expr = if is_async {
                quote! { #fn_name(value, trigger_id, #(#di_call_args.clone()),*).await }
            } else if allow_sync_present {
                quote! { #fn_name(value, trigger_id, #(#di_call_args.clone()),*) }
            } else {
                quote! {
                    {
                        tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                        #fn_name(value, trigger_id, #(#di_call_args.clone()),*)
                    }
                }
            };
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Broadcast Queue trigger - each handler subscribes independently
                // Subscribe to get our own receiver (async)
                let mut receiver = #target_type::subscribe().await;

                loop {
                    match receiver.recv().await {
                        Ok(value) => {
                            let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                            tracing::info!("Queue trigger '{}' received item (ID: {})", #fn_name_str, trigger_id);
                            if let Err(e) = #call_expr {
                                tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Queue trigger '{}' lagged by {} messages", #fn_name_str, n);
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::warn!("Queue trigger '{}' channel closed", #fn_name_str);
                            break;
                        }
                    }
                }
            }
        }
        // Load-balancing queue trigger - all triggers share one receiver
        "lb_queue" => {
            let call_expr = if is_async {
                quote! { #fn_name(value, trigger_id, #(#di_call_args.clone()),*).await }
            } else if allow_sync_present {
                quote! { #fn_name(value, trigger_id, #(#di_call_args.clone()),*) }
            } else {
                quote! {
                    {
                        tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                        #fn_name(value, trigger_id, #(#di_call_args.clone()),*)
                    }
                }
            };
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // LB Queue trigger - use shared receiver via lock (async)
                let queue_wrapper = <#target_type as service_daemon::Provided>::resolve().await;

                loop {
                    let item = {
                        let mut receiver = queue_wrapper.rx.lock().await;
                        receiver.recv().await
                    };

                    match item {
                        Some(value) => {
                            let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                            tracing::info!("LB Queue trigger '{}' received item (ID: {})", #fn_name_str, trigger_id);
                            if let Err(e) = #call_expr {
                                tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                            }
                        }
                        None => {
                            tracing::warn!("LB Queue trigger '{}' channel closed", #fn_name_str);
                            break;
                        }
                    }
                }
            }
        }

        "cron" => {
            // For cron, we need to clone the Arc values into the closure
            let di_clone_for_cron: Vec<_> = di_call_args
                .iter()
                .map(|arg| {
                    quote! { let #arg = #arg.clone(); }
                })
                .collect();

            let call_expr = if is_async {
                quote! { #fn_name((), trigger_id, #(#di_call_args),*).await }
            } else if allow_sync_present {
                quote! { #fn_name((), trigger_id, #(#di_call_args),*) }
            } else {
                quote! {
                    {
                        tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                        #fn_name((), trigger_id, #(#di_call_args),*)
                    }
                }
            };

            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Cron trigger - resolve target using Type-Based DI (async)
                use service_daemon::tokio_cron_scheduler::{Job, JobScheduler};

                let schedule_wrapper = <#target_type as service_daemon::Provided>::resolve().await;

                let sched = JobScheduler::new().await?;

                let fn_name_for_job = #fn_name_str;
                #(#di_clone_for_cron)*
                let job = Job::new_async(schedule_wrapper.as_str(), move |_uuid, _lock| {
                    let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                    let fn_name_clone = fn_name_for_job;
                    #(let #di_call_args = #di_call_args.clone();)*
                    Box::pin(async move {
                        tracing::info!("Cron trigger '{}' fired (ID: {})", fn_name_clone, trigger_id);
                        if let Err(e) = #call_expr {
                            tracing::error!("Trigger '{}' error: {:?}", fn_name_clone, e);
                        }
                    })
                })?;

                sched.add(job).await?;
                sched.start().await?;

                // Keep the scheduler running
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                }
            }
        }
        _ => {
            quote! {
                anyhow::bail!("Unknown trigger template: {}", #template);
            }
        }
    };

    let expanded = quote! {
        #(#attrs)*
        #vis #sig {
            #body
        }

        /// Auto-generated trigger wrapper - acts as an event-loop "Call Host"
        /// This is registered as a Service, so it benefits from ServiceDaemon's lifecycle management.
        pub fn #wrapper_name() -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #event_loop
                #[allow(unreachable_code)]
                Ok(())
            })
        }

        /// Auto-generated static registry entry - triggers are specialized services
        #[service_daemon::linkme::distributed_slice(service_daemon::SERVICE_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ServiceEntry = service_daemon::ServiceEntry {
            name: #fn_name_str,
            module: concat!("triggers/", #template),
            params: &[#(#param_entries),*],
            wrapper: #wrapper_name,
        };
    };

    TokenStream::from(expanded)
}

fn parse_trigger_attr(attr_str: &str, key: &str) -> Option<String> {
    // Parse: key = "value"
    for part in attr_str.split(',') {
        let part = part.trim();
        if part.contains(key) {
            if let Some(value_part) = part.split('=').nth(1) {
                return Some(value_part.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

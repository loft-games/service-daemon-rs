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
        if let FnArg::Typed(syn::PatType { ty, .. }) = arg {
            Some(ty.clone())
        } else {
            None
        }
    });

    let payload_type_token = payload_type
        .clone()
        .unwrap_or_else(|| Box::new(syn::parse_quote!(())));
    let payload_type_str = quote!(#payload_type_token).to_string().replace(" ", "");

    // Compute normalized template name and expected target type
    let normalized_template = match template.as_str() {
        "Notify" | "Event" | "Custom" => "notify",
        "Queue" | "BQueue" | "BroadcastQueue" => "queue",
        "LBQueue" | "LoadBalancingQueue" => "lb_queue",
        "Cron" => "cron",
        _ => &template,
    };

    let _target_type_str = match normalized_template {
        "notify" => "tokio::sync::Notify".to_string(),
        "queue" => format!(
            "tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{}>>",
            payload_type_str
        ),
        "cron" => "String".to_string(),
        "lb_queue" => format!(
            "tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{}>>",
            payload_type_str
        ),
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

        if let FnArg::Typed(syn::PatType { pat, ty, .. }) = arg
            && let Pat::Ident(pat_ident) = &**pat
        {
            let arg_name = &pat_ident.ident;
            let arg_type = ty;
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
            if let Type::Path(syn::TypePath { path, .. }) = &**arg_type
                && let (Some(segment), true) = (path.segments.last(), path.segments.len() == 1)
                && segment.ident == "Arc"
                && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
            {
                // Type-Based DI: use T::resolve().await for async resolution
                di_resolve_tokens.push(quote! {
                    let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                });
                di_call_args.push(quote! { #arg_name });
                continue;
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

    let wrapper_name = format_ident!("{}_trigger_wrapper", fn_name);
    let entry_name = format_ident!("__TRIGGER_ENTRY_{}", fn_name.to_string().to_uppercase());

    let is_async = input.sig.asyncness.is_some();
    let allow_sync_present = has_allow_sync(attrs);

    // Generate template-specific event loop call
    let event_loop_call = match normalized_template {
        // Signal/Notify triggers
        "notify" => {
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
                #(#di_resolve_tokens)*
                let notifier_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::signal_trigger_host(
                    #fn_name_str,
                    notifier_wrapper.0.clone(),
                    move |trigger_id| {
                        #(let #di_call_args = #di_call_args.clone();)*
                        Box::pin(async move {
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "queue" => {
            let call_expr = if is_async {
                quote! { #fn_name(value, trigger_id, #(#di_call_args),*).await }
            } else if allow_sync_present {
                quote! { #fn_name(value, trigger_id, #(#di_call_args),*) }
            } else {
                quote! {
                    {
                        tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                        #fn_name(value, trigger_id, #(#di_call_args),*)
                    }
                }
            };
            quote! {
                #(#di_resolve_tokens)*
                let receiver = #target_type::subscribe().await;
                service_daemon::utils::triggers::queue_trigger_host(
                    #fn_name_str,
                    receiver,
                    move |value, trigger_id| {
                        #(let #di_call_args = #di_call_args.clone();)*
                        Box::pin(async move {
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "lb_queue" => {
            let call_expr = if is_async {
                quote! { #fn_name(value, trigger_id, #(#di_call_args),*).await }
            } else if allow_sync_present {
                quote! { #fn_name(value, trigger_id, #(#di_call_args),*) }
            } else {
                quote! {
                    {
                        tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                        #fn_name(value, trigger_id, #(#di_call_args),*)
                    }
                }
            };
            quote! {
                #(#di_resolve_tokens)*
                let queue_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::lb_queue_trigger_host(
                    #fn_name_str,
                    queue_wrapper.rx.clone(),
                    move |value, trigger_id| {
                        #(let #di_call_args = #di_call_args.clone();)*
                        Box::pin(async move {
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "cron" => {
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
                #(#di_resolve_tokens)*
                let schedule_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::cron_trigger_host(
                    #fn_name_str,
                    schedule_wrapper.as_str(),
                    move |trigger_id| {
                        #(let #di_call_args = #di_call_args.clone();)*
                        Box::pin(async move {
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
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
        pub fn #wrapper_name(token: service_daemon::tokio_util::sync::CancellationToken) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #event_loop_call
            })
        }

        /// Auto-generated static registry entry - triggers are specialized services
        #[service_daemon::linkme::distributed_slice(service_daemon::SERVICE_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ServiceEntry = service_daemon::ServiceEntry {
            name: #fn_name_str,
            module: concat!("triggers/", #normalized_template),
            params: &[#(#param_entries),*],
            wrapper: #wrapper_name,
        };
    };

    TokenStream::from(expanded)
}

fn parse_trigger_attr(attr_str: &str, key: &str) -> Option<String> {
    // Parse: key = "value"
    attr_str.split(',').find_map(|part| {
        let part = part.trim();
        if part.contains(key)
            && let Some((_, val)) = part.split_once('=')
        {
            Some(val.trim().to_string())
        } else {
            None
        }
    })
}

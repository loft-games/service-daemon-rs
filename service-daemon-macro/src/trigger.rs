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

    // Extract parameters and categorize them
    let mut di_resolve_tokens = Vec::new();
    let mut di_capture_idents = Vec::new(); // Variables that need to be cloned into the closure
    let mut call_args = Vec::new(); // Arguments passed to the user function
    let mut param_entries = Vec::new(); // Registry metadata
    let mut payload_arg_name = None;

    for arg in sig.inputs.iter() {
        if let FnArg::Typed(syn::PatType {
            pat,
            ty,
            attrs: arg_attrs,
            ..
        }) = arg
            && let Pat::Ident(pat_ident) = &**pat
        {
            let arg_name = &pat_ident.ident;
            let arg_type = ty;
            let arg_name_str = arg_name.to_string();
            let arg_type_str = quote!(#arg_type).to_string().replace(" ", "");

            // 1. Check if explicitly marked as #[payload]
            let is_explicit_payload = arg_attrs.iter().any(|a| a.path().is_ident("payload"));

            // 2. Check if it's an Arc<T>
            let is_arc = if let Type::Path(syn::TypePath { path, .. }) = &**arg_type
                && let (Some(segment), true) = (path.segments.last(), path.segments.len() == 1)
                && segment.ident == "Arc"
            {
                true
            } else {
                false
            };

            // Categorization logic:
            // - If #[payload] is present, it's the payload regardless of type.
            // - If it's NOT an Arc, it's the payload (only one allowed).
            // - Otherwise, it's a DI Resource.
            if is_explicit_payload || (!is_arc && payload_arg_name.is_none()) {
                if payload_arg_name.is_some() {
                    abort!(
                        arg,
                        "Multiple payload parameters detected. Only one parameter can be the event payload."
                    );
                }
                payload_arg_name = Some(arg_name.clone());

                // If the user wants Arc<Payload>, we must wrap it
                if is_arc {
                    call_args.push(quote! { std::sync::Arc::new(payload) });
                } else {
                    call_args.push(quote! { payload });
                }
            } else if is_arc {
                // It's a DI Resource
                if let Type::Path(syn::TypePath { path, .. }) = &**arg_type
                    && let (Some(segment), _) = (path.segments.last(), true)
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
                {
                    di_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                    });
                    di_capture_idents.push(quote! { #arg_name });
                    call_args.push(quote! { #arg_name });

                    let key_str = format!("{}_{}", arg_name_str, arg_type_str);
                    param_entries.push(quote! {
                        service_daemon::ServiceParam {
                            name: #arg_name_str,
                            type_name: #arg_type_str,
                            key: #key_str,
                        }
                    });
                }
            } else {
                abort!(
                    arg,
                    "Parameter must be either Arc<T> for DI or a payload parameter."
                );
            }
        }
    }

    // Compute normalized template name
    let normalized_template = match template.as_str() {
        "Notify" | "Event" | "Custom" => "notify",
        "Queue" | "BQueue" | "BroadcastQueue" => "queue",
        "LBQueue" | "LoadBalancingQueue" => "lb_queue",
        "Cron" => "cron",
        _ => &template,
    };

    let wrapper_name = format_ident!("{}_trigger_wrapper", fn_name);
    let entry_name = format_ident!("__TRIGGER_ENTRY_{}", fn_name.to_string().to_uppercase());

    let is_async = input.sig.asyncness.is_some();
    let allow_sync_present = has_allow_sync(attrs);

    // Prepare the user function call
    let call_expr = if is_async {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Trigger '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str);
                #fn_name(#(#call_args),*)
            }
        }
    };

    // Generate template-specific event loop call
    let event_loop_call = match normalized_template {
        "notify" => {
            quote! {
                #(#di_resolve_tokens)*
                let notifier_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::signal_trigger_host(
                    #fn_name_str,
                    notifier_wrapper.0.clone(),
                    move || {
                        #(let #di_capture_idents = #di_capture_idents.clone();)*
                        Box::pin(async move {
                            let payload = (); // Dummy for consistency
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "queue" => {
            quote! {
                #(#di_resolve_tokens)*
                let receiver = #target_type::subscribe().await;
                service_daemon::utils::triggers::queue_trigger_host(
                    #fn_name_str,
                    receiver,
                    move |payload| {
                        #(let #di_capture_idents = #di_capture_idents.clone();)*
                        Box::pin(async move {
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "lb_queue" => {
            quote! {
                #(#di_resolve_tokens)*
                let queue_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::lb_queue_trigger_host(
                    #fn_name_str,
                    queue_wrapper.rx.clone(),
                    move |payload| {
                        #(let #di_capture_idents = #di_capture_idents.clone();)*
                        Box::pin(async move {
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "cron" => {
            quote! {
                #(#di_resolve_tokens)*
                let schedule_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::cron_trigger_host(
                    #fn_name_str,
                    schedule_wrapper.as_str(),
                    move || {
                        #(let #di_capture_idents = #di_capture_idents.clone();)*
                        Box::pin(async move {
                            let payload = (); // Dummy for consistency
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

    // Generate a "clean" signature (removing #[payload] attributes)
    let mut clean_sig = sig.clone();
    for arg in clean_sig.inputs.iter_mut() {
        if let FnArg::Typed(syn::PatType {
            attrs: arg_attrs, ..
        }) = arg
        {
            arg_attrs.retain(|a| !a.path().is_ident("payload"));
        }
    }

    let expanded = quote! {
        #(#attrs)*
        #vis #clean_sig {
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

//! `#[trigger]` macro implementation.

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote, quote_spanned};
use syn::{ItemFn, parse_macro_input};

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
    let mut di_capture_idents = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();
    let mut mutability_marks = Vec::new();
    let mut payload_arg_name = None;
    let mut _payload_is_arc = false;
    let mut _payload_span = None;

    let mut clean_inputs = syn::punctuated::Punctuated::<syn::FnArg, syn::token::Comma>::new();
    for arg in sig.inputs.iter() {
        if let Some((arg_name, intent)) = crate::common::analyze_param(arg) {
            let arg_name_str = arg_name.to_string();

            match intent {
                crate::common::ParamIntent::Payload { is_arc, span } => {
                    if payload_arg_name.is_some() {
                        abort!(
                            arg,
                            "Multiple payload parameters detected. Only one parameter can be the event payload."
                        );
                    }
                    payload_arg_name = Some(arg_name.clone());
                    _payload_is_arc = is_arc;
                    _payload_span = span;

                    let mut clean_arg = arg.clone();
                    if let syn::FnArg::Typed(syn::PatType { attrs, .. }) = &mut clean_arg {
                        attrs.retain(|a| !a.path().is_ident("payload"));
                    }
                    clean_inputs.push(clean_arg);

                    if is_arc {
                        call_args.push(quote! { std::sync::Arc::new(payload) });
                    } else {
                        call_args.push(quote! { payload });
                    }
                }
                crate::common::ParamIntent::Dependency {
                    inner_type,
                    wrapper,
                } => {
                    let type_str = quote!(#inner_type).to_string().replace(" ", "");
                    let arg_type_wrapper_str = match wrapper {
                        crate::common::WrapperKind::Arc(_) => format!("Arc<{}>", type_str),
                        crate::common::WrapperKind::ArcRwLock(_, _) => {
                            format!("Arc<RwLock<{}>>", type_str)
                        }
                        crate::common::WrapperKind::ArcMutex(_, _) => {
                            format!("Arc<Mutex<{}>>", type_str)
                        }
                    };

                    match wrapper {
                        crate::common::WrapperKind::Arc(arc_span) => {
                            di_resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                            });
                            clean_inputs.push(
                                syn::parse2(quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#inner_type> })
                                    .unwrap(),
                            );
                        }
                        crate::common::WrapperKind::ArcRwLock(arc_span, rwlock_span) => {
                            di_resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve_rwlock().await;
                            });
                            let rw_path = quote_spanned! { rwlock_span => service_daemon::utils::managed_state::RwLock<#inner_type> };
                            clean_inputs.push(syn::parse2(quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#rw_path> }).unwrap());
                            let mark_name = format_ident!(
                                "__MUT_MARK_{}_{}",
                                fn_name.to_string().to_uppercase(),
                                arg_name.to_string().to_uppercase()
                            );
                            mutability_marks.push(quote! {
                                #[service_daemon::linkme::distributed_slice(service_daemon::models::mutability::MUTABILITY_REGISTRY)]
                                #[linkme(crate = service_daemon::linkme)]
                                static #mark_name: service_daemon::models::mutability::MutabilityMark = service_daemon::models::mutability::MutabilityMark {
                                    key: #type_str
                                };
                            });
                        }
                        crate::common::WrapperKind::ArcMutex(arc_span, mutex_span) => {
                            di_resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve_mutex().await;
                            });
                            let mutex_path = quote_spanned! { mutex_span => service_daemon::utils::managed_state::Mutex<#inner_type> };
                            clean_inputs.push(syn::parse2(quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#mutex_path> }).unwrap());
                            let mark_name = format_ident!(
                                "__MUT_MARK_{}_{}",
                                fn_name.to_string().to_uppercase(),
                                arg_name.to_string().to_uppercase()
                            );
                            mutability_marks.push(quote! {
                                #[service_daemon::linkme::distributed_slice(service_daemon::models::mutability::MUTABILITY_REGISTRY)]
                                #[linkme(crate = service_daemon::linkme)]
                                static #mark_name: service_daemon::models::mutability::MutabilityMark = service_daemon::models::mutability::MutabilityMark {
                                    key: #type_str
                                };
                            });
                        }
                    }

                    di_capture_idents.push(quote! { #arg_name });
                    call_args.push(quote! { #arg_name });
                    let key_str = format!("{}_{}", arg_name_str, arg_type_wrapper_str);
                    param_entries.push(quote! {
                        service_daemon::ServiceParam { name: #arg_name_str, type_name: #type_str, key: #key_str }
                    });
                }
            }
            continue;
        }

        abort!(arg, "Unsupported parameter type in trigger.");
    }

    // Compute normalized template name
    let normalized_template = match template.as_str() {
        "Notify" | "Event" | "Custom" => "notify",
        "Queue" | "BQueue" | "BroadcastQueue" => "queue",
        "LBQueue" | "LoadBalancingQueue" => "lb_queue",
        "Cron" => "cron",
        "Watch" | "State" => "watch",
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
                let notifier_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::signal_trigger_host(
                    #fn_name_str,
                    notifier_wrapper.0.clone(),
                    move || {
                        Box::pin(async move {
                            #(#di_resolve_tokens)*
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
                let receiver = #target_type::subscribe().await;
                service_daemon::utils::triggers::queue_trigger_host(
                    #fn_name_str,
                    receiver,
                    move |payload| {
                        Box::pin(async move {
                            #(#di_resolve_tokens)*
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "lb_queue" => {
            quote! {
                let queue_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::lb_queue_trigger_host(
                    #fn_name_str,
                    queue_wrapper.rx.clone(),
                    move |payload| {
                        Box::pin(async move {
                            #(#di_resolve_tokens)*
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "cron" => {
            quote! {
                let schedule_wrapper = <#target_type as service_daemon::Provided>::resolve().await;
                service_daemon::utils::triggers::cron_trigger_host(
                    #fn_name_str,
                    schedule_wrapper.as_str(),
                    move || {
                        Box::pin(async move {
                            #(#di_resolve_tokens)*
                            let payload = (); // Dummy for consistency
                            #call_expr
                        })
                    },
                    token.clone()
                ).await
            }
        }
        "watch" => {
            quote! {
                service_daemon::utils::triggers::watch_trigger_host::<#target_type, _>(
                    #fn_name_str,
                    move |payload| {
                        Box::pin(async move {
                            #(#di_resolve_tokens)*
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

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let expanded = quote! {
        #(#attrs)*
        #vis #clean_sig {
            // "Macro Illusion": Redirect RwLock/Mutex to our tracked versions
            #[allow(unused_imports)]
            use service_daemon::utils::managed_state::{RwLock, Mutex};
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

        #(#mutability_marks)*
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

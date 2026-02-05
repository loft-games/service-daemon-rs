//! Code generation helpers for the `#[trigger]` macro.

use quote::{format_ident, quote};

/// Generates the event loop call code for a specific template type.
///
/// # Arguments
/// - `normalized_template`: The normalized template name (e.g., "notify", "queue").
/// - `fn_name_str`: The function name as a string.
/// - `target_type`: The target type tokens.
/// - `di_resolve_tokens`: Tokens for resolving dependency injection.
/// - `call_expr`: The expression to call the user's function.
/// - `template`: The original template name for error messages.
pub fn generate_event_loop_call(
    normalized_template: &str,
    fn_name_str: &str,
    target_type: &proc_macro2::TokenStream,
    di_resolve_tokens: &[proc_macro2::TokenStream],
    call_expr: &proc_macro2::TokenStream,
    template: &str,
) -> proc_macro2::TokenStream {
    match normalized_template {
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
    }
}

/// Generates the watcher function and pointer for dependency change monitoring.
///
/// # Returns
/// A tuple of `(watcher_fn_tokens, watcher_ptr_tokens)`.
pub fn generate_watcher(
    fn_name: &syn::Ident,
    normalized_template: &str,
    watcher_select_arms: &[proc_macro2::TokenStream],
    target_type: &proc_macro2::TokenStream,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let watcher_name = format_ident!("{}_watcher", fn_name);

    if !watcher_select_arms.is_empty()
        || matches!(
            normalized_template,
            "watch" | "cron" | "queue" | "lb_queue" | "notify"
        )
    {
        let mut final_watcher_arms = watcher_select_arms.to_vec();

        // Automatically watch the target for reloads - this makes triggers
        // reactive to their configuration/provider changes.
        final_watcher_arms.push(quote! {
            _ = <#target_type as service_daemon::Provided>::changed() => {}
        });

        (
            quote! {
                /// Auto-generated watcher for the trigger - notifies when dependencies change
                pub fn #watcher_name() -> service_daemon::futures::future::BoxFuture<'static, ()> {
                    Box::pin(async move {
                        service_daemon::tokio::select! {
                            #(#final_watcher_arms),*
                        }
                    })
                }
            },
            quote! { Some(#watcher_name) },
        )
    } else {
        (quote! {}, quote! { None })
    }
}

/// Generates the call expression for the user's function.
pub fn generate_call_expr(
    fn_name: &syn::Ident,
    fn_name_str: &str,
    call_args: &[proc_macro2::TokenStream],
    is_async: bool,
    allow_sync_present: bool,
) -> proc_macro2::TokenStream {
    if is_async {
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
    }
}

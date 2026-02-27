//! Code generation helpers for the `#[trigger]` macro.
//!
//! The key function [`generate_event_loop_call`] now generates a single, unified
//! `<Host as TriggerHost<Target>>::run_as_service(...)` call regardless of the
//! trigger type. All template-specific logic lives in the runtime `TriggerHost`
//! implementations, not in the macro.

use quote::{format_ident, quote};

/// Generates the unified event loop call code via `TriggerHost::run_as_service`.
///
/// # Arguments
/// - `host_path`: The host type path tokens (e.g., `Notify`, `TT::Queue`).
/// - `fn_name_str`: The function name as a string.
/// - `target_type`: The target type tokens (e.g., `UserNotifier`, `TaskQueue`).
/// - `di_resolve_tokens`: Tokens for resolving additional DI dependencies.
/// - `call_expr`: The expression to call the user's function.
pub fn generate_event_loop_call(
    host_path: &syn::Path,
    fn_name_str: &str,
    target_type: &proc_macro2::TokenStream,
    di_resolve_tokens: &[proc_macro2::TokenStream],
    call_expr: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    // Unified codegen: resolve the target via DI, then delegate to the host's
    // `TriggerHost::run_as_service` implementation. The host type and target
    // type are both provided as-is from the user's attribute — the Rust
    // compiler will verify the trait bound at the call site.
    quote! {
        let target = <#target_type as service_daemon::Provided>::resolve().await;
        <#host_path as service_daemon::TriggerHost<#target_type>>::run_as_service(
            #fn_name_str.to_string(),
            target,
            std::sync::Arc::new(move |ctx| {
                Box::pin(async move {
                    let payload = ctx.message.payload;
                    #(#di_resolve_tokens)*
                    #call_expr
                })
            }),
            token.clone(),
        ).await
    }
}

/// Generates the watcher function and pointer for dependency change monitoring.
///
/// # Returns
/// A tuple of `(watcher_fn_tokens, watcher_ptr_tokens)`.
pub fn generate_watcher(
    fn_name: &syn::Ident,
    watcher_select_arms: &[proc_macro2::TokenStream],
    target_type: &proc_macro2::TokenStream,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let watcher_name = format_ident!("{}_watcher", fn_name);

    // All trigger types should watch their target for configuration changes.
    // This is the unified logic — no need for template-specific branching.
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

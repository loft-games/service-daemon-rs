//! Code generation helpers for the `#[trigger]` macro.
//!
//! Only `generate_event_loop_call` remains trigger-specific.
//! `generate_call_expr` and `generate_watcher` are now shared via `common.rs`.

use quote::quote;

/// Generates the unified event loop call code via `TriggerHost::run_as_service`.
///
/// # DI Resolution Position
///
/// The `di_resolve_tokens` are emitted **outside** the per-event closure,
/// inside the wrapper's top-level `async move` block. This means dependencies
/// are resolved **once** when the trigger service starts, not on every event.
/// This matches the behavior of `#[service]` DI resolution.
///
/// Because the per-event closure has the `Fn` trait (called multiple times),
/// each DI-resolved `Arc<T>` is `.clone()`-d into the closure body via shadow
/// bindings (`let x = x.clone();`). This is an `Arc` reference-count bump —
/// zero allocation, same semantics as the old per-event resolve.
///
/// # Arguments
/// - `host_path`: The host type path tokens (e.g., `Notify`, `TT::Queue`).
/// - `fn_name_str`: The function name as a string.
/// - `target_type`: The target type tokens (e.g., `UserNotifier`, `TaskQueue`).
/// - `di_resolve_tokens`: Tokens for resolving additional DI dependencies (once).
/// - `di_idents`: Variable names of DI-resolved dependencies (for clone lines).
/// - `call_expr`: The expression to call the user's function.
pub fn generate_event_loop_call(
    host_path: &syn::Path,
    fn_name_str: &str,
    target_type: &proc_macro2::TokenStream,
    di_resolve_tokens: &[proc_macro2::TokenStream],
    di_idents: &[syn::Ident],
    call_expr: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    // Generate `let x = x.clone();` shadow bindings for each DI dependency.
    // These clone the Arc into the Fn closure on each invocation.
    let clone_lines: Vec<_> = di_idents
        .iter()
        .map(|ident| quote! { let #ident = #ident.clone(); })
        .collect();

    // DI dependencies are resolved ONCE here (outside the closure),
    // then cloned into the per-event closure via shadow bindings.
    quote! {
        let target = <#target_type as service_daemon::Provided>::resolve().await;
        #(#di_resolve_tokens)*
        <#host_path as service_daemon::TriggerHost<#target_type>>::run_as_service(
            #fn_name_str,
            target,
            std::sync::Arc::new(move |ctx| {
                #(#clone_lines)*
                Box::pin(async move {
                    let payload = ctx.message.payload;
                    #call_expr
                })
            }),
            token.clone(),
        ).await
    }
}

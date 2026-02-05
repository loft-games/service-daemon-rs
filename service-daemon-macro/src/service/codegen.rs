//! Code generation helpers for the `#[service]` macro.

use quote::{format_ident, quote};

/// Generates the watcher function and pointer for dependency change monitoring.
///
/// # Returns
/// A tuple of `(watcher_fn_tokens, watcher_ptr_tokens)`.
pub fn generate_watcher(
    fn_name: &syn::Ident,
    watcher_select_arms: &[proc_macro2::TokenStream],
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let watcher_name = format_ident!("{}_watcher", fn_name);

    if !watcher_select_arms.is_empty() {
        (
            quote! {
                /// Auto-generated watcher for the service - notifies when dependencies change
                pub fn #watcher_name() -> service_daemon::futures::future::BoxFuture<'static, ()> {
                    Box::pin(async move {
                        service_daemon::tokio::select! {
                            #(#watcher_select_arms),*
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
        // User explicitly allowed sync, no warning
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Service '{}' is synchronous. Consider switching to 'async fn' to avoid blocking the executor.", #fn_name_str);
                #fn_name(#(#call_args),*)
            }
        }
    }
}

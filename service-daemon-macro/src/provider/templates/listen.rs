//! TCP listener provider template.

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::super::impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};
use super::context::has_clone_derive;

/// Generates a Listen (TCP Listener) provider with kernel-level FD cloning.
///
/// The generated struct wraps `Arc<std::net::TcpListener>` to satisfy the
/// `Clone` requirement of `Provided`. The `try_new()` constructor performs the
/// fallible bind and classifies OS errors into provider errors. The `get()` method clones the
/// underlying OS socket via `try_clone()` and converts to an async
/// `tokio::net::TcpListener` for each caller, returning OS/runtime errors to
/// the calling service.
///
/// # Generated code shape
///
/// ```rust,ignore
/// pub struct MyListener(pub std::sync::Arc<std::net::TcpListener>);
///
/// impl MyListener {
///     pub fn try_new() -> Result<Self, service_daemon::ProviderError> { /* bind + classify */ }
///     pub fn get(&self) -> std::io::Result<tokio::net::TcpListener> { /* try_clone + from_std */ }
/// }
/// ```
pub(in crate::provider) fn generate_listen_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    addr: &syn::LitStr,
    env: Option<&syn::LitStr>,
    eager: bool,
) -> TokenStream {
    let struct_name_str = struct_name.to_string();

    // Listen uses a custom fallible initializer (bind may fail).
    // We bypass TemplateContext here to control constructor/init generation.
    let clone_derive = if has_clone_derive(attrs) {
        quote! {}
    } else {
        quote! { #[derive(Clone)] }
    };

    // Build the address resolution expression:
    // - With env: try env var first, fall back to the literal default
    // - Without env: use the literal default directly
    let addr_expr = if let Some(env_lit) = env {
        let env_str = env_lit.value();
        quote! {
            std::env::var(#env_str).unwrap_or_else(|_| #addr.to_owned())
        }
    } else {
        quote! { #addr.to_owned() }
    };

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );

    let type_tokens = quote! { #struct_name };
    let framework_init_fn = quote! {
        service_daemon::__private::init_fallible_with_source(
            #struct_name_str,
            policy,
            cancel,
            service_daemon::__private::ProviderInitSourceKind::SystemIoFatal,
            service_daemon::__private::ProviderInitSourceKind::SystemIoRetryable,
            move || async move { #struct_name::try_new() },
        )
        .await
    };
    let managed_init_fn = quote! {
        #struct_name::try_new().map(std::sync::Arc::new)
    };

    let provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &[],
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(Listen)] struct {struct_name}"),
    });

    let expanded = quote! {
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name(pub std::sync::Arc<std::net::TcpListener>);

        impl #struct_name {
            pub fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let addr = #addr_expr;
                let listener = std::net::TcpListener::bind(&addr).map_err(|e| {
                    let msg = format!(
                        "Provider '{}' failed to bind TCP port '{}': {}",
                        #struct_name_str, addr, e
                    );
                    match e.kind() {
                        std::io::ErrorKind::AddrInUse
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut => {
                            service_daemon::ProviderError::Retryable(msg)
                        }
                        _ => service_daemon::ProviderError::Fatal(msg),
                    }
                })?;
                listener.set_nonblocking(true).map_err(|e| {
                    service_daemon::ProviderError::Fatal(format!(
                        "Provider '{}' failed to set nonblocking for '{}': {}",
                        #struct_name_str, addr, e
                    ))
                })?;
                Ok(Self(std::sync::Arc::new(listener)))
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = std::net::TcpListener;
            fn deref(&self) -> &std::net::TcpListener {
                &self.0
            }
        }

        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self.0.local_addr() {
                    Ok(addr) => write!(f, "{}", addr),
                    Err(_) => write!(f, "<unresolved>"),
                }
            }
        }

        #provided_impl

        impl #struct_name {
            /// Obtain an async `tokio::net::TcpListener` by cloning the underlying OS socket.
            ///
            /// Each call creates a new file descriptor via the kernel's `dup` syscall,
            /// allowing multiple services or reload generations to share the same
            /// physical listening port concurrently.
            pub fn get(&self) -> std::io::Result<service_daemon::__private::tokio::net::TcpListener> {
                let cloned = self.0.try_clone()?;
                cloned.set_nonblocking(true)?;
                service_daemon::__private::tokio::net::TcpListener::from_std(cloned)
            }

            /// Returns the local address this listener is bound to.
            pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
                self.0.local_addr()
            }
        }
    };

    TokenStream::from(expanded)
}

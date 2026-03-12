//! Template generators for special provider types.
//!
//! This module contains generators for:
//! - Notify (Signal) template
//! - Broadcast Queue template
//! - Listen (TCP Listener) template
//!
//! Both templates share common initialization logic via [`TemplateContext`].

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::struct_gen::generate_provided_impl;

/// Checks whether any attribute in the list already contains `derive(Clone)`.
///
/// Uses proper AST-based parsing via `syn::punctuated::Punctuated<Path, Token![,]>`
/// to correctly handle complex derive lists (e.g., `derive(MyMacro<A, B>, Clone)`).
fn has_clone_derive(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        // Parse the derive arguments as a comma-separated list of paths.
        // This correctly handles generics with commas (e.g., `MyMacro<A, B>`)
        // unlike the previous string-split approach.
        attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        )
        .is_ok_and(|paths| {
            paths.iter().any(|path| {
                // Match bare `Clone` or qualified `std::clone::Clone` / `core::clone::Clone`
                path.segments.last().is_some_and(|seg| seg.ident == "Clone")
            })
        })
    })
}

/// Shared context for all template-based providers.
///
/// Encapsulates the common boilerplate (singleton name generation, Clone derive
/// detection, constructor, and provider capability impls) that every template
/// needs. Individual templates only supply their struct body and convenience
/// methods.
struct TemplateContext<'a> {
    struct_name: &'a syn::Ident,
    vis: &'a syn::Visibility,
    attrs: &'a [syn::Attribute],
    clone_derive: proc_macro2::TokenStream,
    provided_impl: proc_macro2::TokenStream,
}

impl<'a> TemplateContext<'a> {
    /// Creates a new template context with all common boilerplate pre-computed.
    ///
    /// All templates use `Self::default()` as the constructor. They default to
    /// the full provider capability set: snapshot resolution, managed-state
    /// injection, and watch notifications backed by `StateManager` snapshot
    /// publication.
    fn new(
        struct_name: &'a syn::Ident,
        vis: &'a syn::Visibility,
        attrs: &'a [syn::Attribute],
    ) -> Self {
        let singleton_name = format_ident!(
            "__PROVIDER_SINGLETON_{}",
            struct_name.to_string().to_uppercase()
        );

        let clone_derive = if has_clone_derive(attrs) {
            quote! {}
        } else {
            quote! { #[derive(Clone)] }
        };

        let constructor = quote! { std::sync::Arc::new(#struct_name::default()) };

        let type_tokens = quote! { #struct_name };
        let init_fn = quote! {
            let _ = policy;
            let _ = cancel;
            #constructor
        };

        let provided_impl = generate_provided_impl(
            &type_tokens,
            &singleton_name,
            struct_name.span(),
            &[],
            false,
            &init_fn,
        );

        Self {
            struct_name,
            vis,
            attrs,
            clone_derive,
            provided_impl,
        }
    }
}

/// Generates a Signal provider using `tokio::sync::Notify`.
pub fn generate_notify_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
) -> TokenStream {
    let ctx = TemplateContext::new(struct_name, vis, attrs);
    let TemplateContext {
        struct_name,
        vis,
        attrs,
        clone_derive,
        provided_impl,
        ..
    } = &ctx;

    let expanded = quote! {
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name(pub std::sync::Arc<tokio::sync::Notify>);

        impl Default for #struct_name {
            fn default() -> Self {
                Self(std::sync::Arc::new(tokio::sync::Notify::new()))
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = tokio::sync::Notify;
            fn deref(&self) -> &tokio::sync::Notify {
                &self.0
            }
        }

        #provided_impl

        impl #struct_name {
            /// Trigger this signal, waking all subscribed triggers.
            pub fn notify(&self) {
                self.0.notify_waiters();
            }

            /// Wait for a notification on this signal.
            pub async fn wait(&self) {
                self.0.notified().await;
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Broadcast Queue provider using `tokio::sync::broadcast`.
pub fn generate_broadcast_queue_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    item_type: &syn::Type,
    capacity: usize,
) -> TokenStream {
    let ctx = TemplateContext::new(struct_name, vis, attrs);
    let TemplateContext {
        struct_name,
        vis,
        attrs,
        clone_derive,
        provided_impl,
        ..
    } = &ctx;

    let expanded = quote! {
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            pub tx: tokio::sync::broadcast::Sender<#item_type>,
        }

        impl Default for #struct_name {
            fn default() -> Self {
                let (tx, _) = tokio::sync::broadcast::channel(#capacity);
                Self { tx }
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = tokio::sync::broadcast::Sender<#item_type>;
            fn deref(&self) -> &tokio::sync::broadcast::Sender<#item_type> {
                &self.tx
            }
        }

        #provided_impl

        impl #struct_name {
            /// Push an item to this queue.
            pub fn push(&self, item: #item_type) -> Result<usize, tokio::sync::broadcast::error::SendError<#item_type>> {
                self.tx.send(item)
            }

            /// Subscribe to this queue to receive broadcast messages.
            pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<#item_type> {
                self.tx.subscribe()
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Listen (TCP Listener) provider with kernel-level FD cloning.
///
/// The generated struct wraps `Arc<std::net::TcpListener>` to satisfy the
/// `Clone` requirement of `Provided`. The `Default` impl performs the actual
/// bind with a Fail-fast (panic) strategy. The `get()` method clones the
/// underlying OS socket via `try_clone()` and converts to an async
/// `tokio::net::TcpListener` for each caller.
///
/// # Generated code shape
///
/// ```rust,ignore
/// pub struct MyListener(pub std::sync::Arc<std::net::TcpListener>);
///
/// impl Default for MyListener { /* bind + set_nonblocking + panic on fail */ }
/// impl MyListener {
///     pub fn get(&self) -> tokio::net::TcpListener { /* try_clone + from_std */ }
/// }
/// ```
pub fn generate_listen_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    addr: &syn::LitStr,
    env: Option<&syn::LitStr>,
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
    let init_fn = quote! {
        let addr = #addr_expr;
        service_daemon::core::provider_init::init_fallible(
            policy,
            cancel,
            || async {
                let listener = std::net::TcpListener::bind(&addr)
                    .map_err(|e| service_daemon::ProviderError::Retryable(format!(
                        "Provider '{}' failed to bind TCP port '{}': {}",
                        #struct_name_str, addr, e
                    )))?;
                listener.set_nonblocking(true)
                    .map_err(|e| service_daemon::ProviderError::Fatal(format!(
                        "Provider '{}' failed to set nonblocking for '{}': {}",
                        #struct_name_str, addr, e
                    )))?;
                Ok(#struct_name(std::sync::Arc::new(listener)))
            },
        )
        .await
    };

    let provided_impl = generate_provided_impl(
        &type_tokens,
        &singleton_name,
        struct_name.span(),
        &[],
        false,
        &init_fn,
    );

    let expanded = quote! {
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name(pub std::sync::Arc<std::net::TcpListener>);

        impl Default for #struct_name {
            fn default() -> Self {
                let addr = #addr_expr;
                let listener = std::net::TcpListener::bind(&addr)
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "FATAL: Provider '{}' failed to bind TCP port '{}': {}. Is another process using this port?",
                            #struct_name_str, addr, e
                        );
                        std::process::exit(1)
                    });
                // CRITICAL: Must set nonblocking before converting to tokio,
                // otherwise `tokio::net::TcpListener::from_std` will panic
                // or the event loop will hang indefinitely.
                listener.set_nonblocking(true)
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "FATAL: Provider '{}' failed to set nonblocking for '{}': {}",
                            #struct_name_str, addr, e
                        );
                        std::process::exit(1)
                    });
                Self(std::sync::Arc::new(listener))
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
            ///
            /// # Panics
            /// Panics if the FD clone or the std-to-tokio conversion fails. These
            /// failures indicate OS-level resource exhaustion (e.g., `EMFILE`).
            pub fn get(&self) -> service_daemon::tokio::net::TcpListener {
                let cloned = self.0.try_clone()
                    .expect("Failed to clone TCP listener file descriptor (OS resource exhaustion?)");
                service_daemon::tokio::net::TcpListener::from_std(cloned)
                    .expect("Failed to convert cloned std::net::TcpListener to tokio (nonblocking not set?)")
            }

            /// Returns the local address this listener is bound to.
            pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
                self.0.local_addr()
            }
        }
    };

    TokenStream::from(expanded)
}

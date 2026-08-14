//! Unix domain socket provider templates.

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::super::impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};
use super::super::parser::StringTemplateArg;
use super::context::has_clone_derive;

// ---------------------------------------------------------------------------
// Shared helpers for path-based templates (UnixListen / UnixConnect)
// ---------------------------------------------------------------------------

/// Builds the runtime address resolution expression shared by the Unix-socket
/// templates: env-var override wins, literal default is the fallback.
//
// Unix listen/connect templates share this path resolution. TCP listen keeps
// its own address resolution so this split does not alter TCP expansion.
fn unix_path_addr_expr(
    addr: &StringTemplateArg,
    env: Option<&StringTemplateArg>,
) -> proc_macro2::TokenStream {
    let fallback = addr.to_owned_expr();
    if let Some(env_arg) = env {
        let env_expr = env_arg.to_static_str_expr();
        quote! {
            std::env::var(#env_expr).unwrap_or_else(|_| #fallback)
        }
    } else {
        fallback
    }
}

/// Emits a `#[cfg(not(unix))] compile_error!(...)` guard for Unix-only
/// templates so non-Unix builds get a single targeted diagnostic instead of a
/// cascade of "type not found" errors from `std::os::unix::net::*` references.
//
// All generated items are also `#[cfg(unix)]`-gated; on non-Unix targets
// every gated item vanishes and only this `compile_error!` remains. Users
// wanting cross-platform code should wrap the `#[provider(UnixListen|...)]`
// declaration in their own `#[cfg(unix)] mod {...}` -- the outer cfg
// short-circuits this guard cleanly because cfg evaluation is hierarchical
// over macro expansion output.
fn unix_only_compile_error_guard(template_name: &str) -> proc_macro2::TokenStream {
    let message = format!(
        "`{}` provider template is only available on Unix targets. \
         Wrap the `#[provider({}(\"...\"))]` declaration in `#[cfg(unix)]`, \
         or for TCP use the cross-platform `Listen` template instead.",
        template_name, template_name
    );
    quote! {
        #[cfg(not(unix))]
        const _: () = {
            ::std::compile_error!(#message);
        };
    }
}

// ---------------------------------------------------------------------------
// UnixListen template
// ---------------------------------------------------------------------------

/// Generates a `UnixListen` (Unix domain socket listener) provider with
/// FD cloning across reload generations.
//
// Mirrors `generate_listen_template` while preserving Unix-specific behavior:
// stale socket probing, explicit nonblocking setup on cloned descriptors, and
// Unix-only cfg diagnostics.
pub(in crate::provider) fn generate_unix_listen_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    addr: &StringTemplateArg,
    env: Option<&StringTemplateArg>,
    eager: bool,
) -> TokenStream {
    let struct_name_str = struct_name.to_string();
    let clone_derive = if has_clone_derive(attrs) {
        quote! {}
    } else {
        quote! { #[derive(Clone)] }
    };

    let addr_expr = unix_path_addr_expr(addr, env);
    let compile_error_guard = unix_only_compile_error_guard("UnixListen");

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );
    let type_tokens = quote! { #struct_name };

    // Detect-and-unlink preamble + bind + set_nonblocking. Used by both the
    // framework path (init_fallible) and the managed path (sync block).
    //
    // A failed live-process probe is treated as stale only after the path is
    // confirmed to be a Unix socket; ordinary files are preserved.
    let bind_prelude = quote! {
        let p = std::path::Path::new(&path);
        match std::fs::symlink_metadata(p) {
            Ok(_) => {
                match std::os::unix::net::UnixStream::connect(p) {
                    Ok(_probe_stream) => {
                        return Err(service_daemon::ProviderError::Fatal(format!(
                            "Provider '{}': socket '{}' is held by another live process; refusing to bind",
                            #struct_name_str, path,
                        )));
                    }
                    Err(_probe_err) => {
                        let should_remove_stale_socket = match std::fs::symlink_metadata(p) {
                            Ok(metadata) => {
                                let file_type = metadata.file_type();
                                if !std::os::unix::fs::FileTypeExt::is_socket(&file_type) {
                                    return Err(service_daemon::ProviderError::Fatal(format!(
                                        "Provider '{}': path '{}' exists but is not a Unix socket; refusing to remove",
                                        #struct_name_str, path,
                                    )));
                                }
                                true
                            }
                            Err(metadata_err)
                                if metadata_err.kind() == std::io::ErrorKind::NotFound =>
                            {
                                false
                            }
                            Err(metadata_err) => {
                                return Err(service_daemon::ProviderError::Fatal(format!(
                                    "Provider '{}': failed to inspect existing socket path '{}': {} (kind={:?})",
                                    #struct_name_str, path, metadata_err, metadata_err.kind(),
                                )));
                            }
                        };

                        if should_remove_stale_socket {
                            if let Err(remove_err) = std::fs::remove_file(p) {
                                if remove_err.kind() != std::io::ErrorKind::NotFound {
                                    return Err(service_daemon::ProviderError::Fatal(format!(
                                        "Provider '{}': failed to remove stale socket '{}': {} (kind={:?})",
                                        #struct_name_str, path, remove_err, remove_err.kind(),
                                    )));
                                }
                            }
                            // Log stale socket cleanup for operator diagnostics.
                            ::tracing::warn!(
                                provider = #struct_name_str,
                                path = %path,
                                "Removed stale Unix socket file before binding"
                            );
                        }
                    }
                }
            }
            Err(metadata_err) if metadata_err.kind() == std::io::ErrorKind::NotFound => {}
            Err(metadata_err) => {
                return Err(service_daemon::ProviderError::Fatal(format!(
                    "Provider '{}': failed to inspect existing socket path '{}': {} (kind={:?})",
                    #struct_name_str, path, metadata_err, metadata_err.kind(),
                )));
            }
        }
    };

    // Classify io::ErrorKind for the framework's retry/backoff vocabulary.
    // Explicit match arms keep retry classifications deliberate.
    let bind_and_classify = quote! {
        let listener = std::os::unix::net::UnixListener::bind(&path).map_err(|e| {
            let msg = format!(
                "Provider '{}' failed to bind Unix socket '{}': {}",
                #struct_name_str, path, e
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
                #struct_name_str, path, e
            ))
        })?;
    };

    // Framework path: init_fallible wraps the failable operation with backoff,
    // total timeout, and cancellation.
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
        cache_scope: quote! { service_daemon::__private::ProviderCacheScope::Inherited },
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(UnixListen)] struct {struct_name}"),
    });

    let expanded = quote! {
        #compile_error_guard

        // Wrapped Arc<UnixListener> so multiple reload generations can share
        // one listening queue via dup()/try_clone(). The kernel listen socket
        // is stateless from accept()'s perspective: every call draws from a
        // single shared backlog regardless of which clone makes the call.
        #[cfg(unix)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name(pub std::sync::Arc<std::os::unix::net::UnixListener>);

        #[cfg(unix)]
        impl #struct_name {
            pub fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let path = #addr_expr;
                #bind_prelude
                #bind_and_classify
                Ok(Self(std::sync::Arc::new(listener)))
            }
        }

        #[cfg(unix)]
        impl std::ops::Deref for #struct_name {
            type Target = std::os::unix::net::UnixListener;
            fn deref(&self) -> &std::os::unix::net::UnixListener {
                &self.0
            }
        }

        // Display via as_pathname: std::os::unix::net::SocketAddr does NOT
        // implement Display itself (only Debug). For unnamed sockets
        // as_pathname returns None.
        #[cfg(unix)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self.0.local_addr() {
                    Ok(addr) => match addr.as_pathname() {
                        Some(p) => write!(f, "{}", p.display()),
                        None => write!(f, "<unnamed>"),
                    },
                    Err(_) => write!(f, "<unresolved>"),
                }
            }
        }

        #[cfg(unix)]
        #provided_impl

        #[cfg(unix)]
        impl #struct_name {
            /// Obtain an async `tokio::net::UnixListener` by cloning the
            /// underlying OS file descriptor.
            ///
            /// Each call returns a new `tokio::net::UnixListener` that shares
            /// the same kernel listen queue. The kernel distributes
            /// incoming connections fairly across all clones, enabling
            /// multiple services or reload generations to accept on the
            /// same physical path concurrently.
            //
            // We explicitly set_nonblocking(true) on the cloned FD because
            // POSIX dup() is not guaranteed to inherit O_NONBLOCK across libc
            // implementations, so relying on inheritance is not portable.
            pub fn get(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixListener> {
                let cloned = self.0.try_clone()?;
                cloned.set_nonblocking(true)?;
                service_daemon::__private::tokio::net::UnixListener::from_std(cloned)
            }

            async fn accept_raw(&self) -> std::io::Result<(
                service_daemon::__private::tokio::net::UnixStream,
                service_daemon::__private::tokio::net::unix::SocketAddr,
            )> {
                self.get()?.accept().await
            }

            /// Accept one connection as a platform-neutral local IPC stream.
            pub async fn accept(&self) -> std::io::Result<service_daemon::IpcStream> {
                let (stream, _) = self.accept_raw().await?;
                Ok(service_daemon::IpcStream::Unix(stream))
            }

            /// Returns the local address this socket is bound to.
            pub fn local_addr(&self) -> std::io::Result<std::os::unix::net::SocketAddr> {
                self.0.local_addr()
            }
        }
    };

    TokenStream::from(expanded)
}

// ---------------------------------------------------------------------------
// UnixConnect template
// ---------------------------------------------------------------------------

/// Generates a `UnixConnect` (Unix domain socket client) provider that holds
/// only the target path and produces fresh `tokio::net::UnixStream`s on demand.
//
// UnixStream is stateful, so each call opens an independent stream instead of
// sharing a cloned descriptor. Initialization probes reachability once and
// drops the probe stream immediately.
pub(in crate::provider) fn generate_unix_connect_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    addr: &StringTemplateArg,
    env: Option<&StringTemplateArg>,
    eager: bool,
) -> TokenStream {
    let struct_name_str = struct_name.to_string();
    let clone_derive = if has_clone_derive(attrs) {
        quote! {}
    } else {
        quote! { #[derive(Clone)] }
    };

    let addr_expr = unix_path_addr_expr(addr, env);
    let compile_error_guard = unix_only_compile_error_guard("UnixConnect");

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );
    let type_tokens = quote! { #struct_name };

    // Framework path: init_fallible provides retry/backoff/timeout. The
    // returned Arc<Self> caches only the path; subsequent try_connect()
    // calls open fresh streams.
    let framework_init_fn = quote! {
        service_daemon::__private::init_fallible_with_source(
            #struct_name_str,
            policy,
            cancel,
            service_daemon::__private::ProviderInitSourceKind::SystemIoFatal,
            service_daemon::__private::ProviderInitSourceKind::SystemIoRetryable,
            move || async move { #struct_name::try_new().await },
        )
        .await
    };

    let managed_init_fn = quote! {
        #struct_name::try_new().await.map(std::sync::Arc::new)
    };

    let provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &[],
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        cache_scope: quote! { service_daemon::__private::ProviderCacheScope::Inherited },
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(UnixConnect)] struct {struct_name}"),
    });

    let expanded = quote! {
        #compile_error_guard

        // Holds Arc<PathBuf>, NOT the connected stream. UnixStream is
        // stateful; sharing one across callers would corrupt read-side
        // framing. Each try_connect() establishes a fresh independent stream.
        #[cfg(unix)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            path: std::sync::Arc<std::path::PathBuf>,
        }


        #[cfg(unix)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.path.display())
            }
        }

        #[cfg(unix)]
        #provided_impl

        #[cfg(unix)]
        impl #struct_name {
            pub async fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let path = #addr_expr;
                Ok(Self {
                    path: std::sync::Arc::new(std::path::PathBuf::from(path)),
                })
            }

            /// Open a fresh connection to the configured Unix socket.
            ///
            /// Each call establishes an independent `tokio::net::UnixStream`.
            /// Callers needing a long-lived connection should hold the
            /// returned stream themselves; the framework does not pool because
            /// UDS connections are local and cheap.
            pub async fn try_connect(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixStream> {
                service_daemon::__private::tokio::net::UnixStream::connect(&*self.path).await
            }

            /// Open a fresh connection as a platform-neutral local IPC stream.
            pub async fn connect(&self) -> std::io::Result<service_daemon::IpcStream> {
                self.try_connect().await.map(service_daemon::IpcStream::Unix)
            }

            /// Returns the configured socket path.
            pub fn path(&self) -> &std::path::Path {
                &self.path
            }
        }
    };

    TokenStream::from(expanded)
}

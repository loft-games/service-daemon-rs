//! Template generators for built-in provider forms.
//!
//! This module contains generators for:
//! - Notify (Signal) template
//! - Broadcast Queue template
//! - Listen (TCP Listener) template
//!
//! Templates share common initialization logic via [`TemplateContext`].

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::struct_gen::{HelperStyle, ProvidedImplConfig, generate_provided_impl};

/// Parses `derive(...)` attributes and checks whether `Clone` is present.
/// This handles derive entries with generic arguments, such as
/// `derive(MyMacro<A, B>, Clone)`.
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
/// Encapsulates the common boilerplate (root manager name generation, Clone derive
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
    /// In-memory templates use `Self::default()` as the constructor. They default
    /// to the full provider capability set: snapshot resolution, managed-state
    /// injection, and watch notifications backed by `StateManager` snapshot
    /// publication.
    fn new(
        struct_name: &'a syn::Ident,
        vis: &'a syn::Visibility,
        attrs: &'a [syn::Attribute],
        eager: bool,
        helper_style: HelperStyle,
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

        let type_tokens = quote! { #struct_name };
        let framework_init_fn = quote! {
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#struct_name::default()))
        };
        let managed_init_fn = quote! {
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#struct_name::default()))
        };

        let provided_impl = generate_provided_impl(ProvidedImplConfig {
            type_tokens: &type_tokens,
            singleton_name: &singleton_name,
            user_span: struct_name.span(),
            param_entries: &[],
            eager,
            framework_init_fn: &framework_init_fn,
            managed_init_fn: &managed_init_fn,
            helper_style,
        });

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
    eager: bool,
) -> TokenStream {
    let ctx = TemplateContext::new(struct_name, vis, attrs, eager, HelperStyle::Infallible);
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
        #vis struct #struct_name(pub std::sync::Arc<::service_daemon::TrackedNotify>);

        impl Default for #struct_name {
            fn default() -> Self {
                Self(std::sync::Arc::new(::service_daemon::TrackedNotify::new()))
            }
        }

        impl ::std::ops::Deref for #struct_name {
            type Target = ::service_daemon::TrackedNotify;
            fn deref(&self) -> &::service_daemon::TrackedNotify {
                &*self.0
            }
        }

        #provided_impl

        impl #struct_name {
            /// Trigger this signal and wake subscribed triggers.
            /// The tracked signal records a UUID v7 message ID for causal tracing.
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
    capacity: std::num::NonZeroUsize,
    eager: bool,
) -> TokenStream {
    let capacity = capacity.get();
    let ctx = TemplateContext::new(struct_name, vis, attrs, eager, HelperStyle::Infallible);
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
            pub tx: service_daemon::TrackedSender<#item_type>,
        }

        impl Default for #struct_name {
            fn default() -> Self {
                const CAPACITY: std::num::NonZeroUsize = match std::num::NonZeroUsize::new(#capacity) {
                    Some(capacity) => capacity,
                    None => panic!("Queue provider capacity must be greater than zero"),
                };

                Self {
                    tx: service_daemon::TrackedSender::new(CAPACITY),
                }
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = service_daemon::TrackedSender<#item_type>;
            fn deref(&self) -> &service_daemon::TrackedSender<#item_type> {
                &self.tx
            }
        }

        #provided_impl

        impl #struct_name {
            /// Push an item to this queue.
            /// The tracked sender records a UUID v7 message ID for causal tracing.
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
pub fn generate_listen_template(
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
        service_daemon::__private::init_fallible(
            #struct_name_str,
            policy,
            cancel,
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
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
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

// ---------------------------------------------------------------------------
// Shared helpers for path-based templates (Listen / UnixListen / UnixConnect)
// ---------------------------------------------------------------------------

/// Builds the runtime address resolution expression shared by the Unix-socket
/// templates: env-var override wins, literal default is the fallback.
//
// Why a free function: `generate_unix_listen_template` and
// `generate_unix_connect_template` both need this exact resolution shape, and
// keeping a single source prevents drift if env semantics ever change. We
// intentionally leave `generate_listen_template` (TCP) using its inline copy
// so this helper's first commit doesn't risk altering TCP expansion -- the
// duplication is small (~5 lines) and isolated.
fn unix_path_addr_expr(addr: &syn::LitStr, env: Option<&syn::LitStr>) -> proc_macro2::TokenStream {
    if let Some(env_lit) = env {
        let env_str = env_lit.value();
        quote! {
            std::env::var(#env_str).unwrap_or_else(|_| #addr.to_owned())
        }
    } else {
        quote! { #addr.to_owned() }
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
// Mirrors `generate_listen_template` (TCP) in shape but adapts semantics:
//   1. Stale-socket detection. UDS `AddrInUse` often means a leftover
//      socket file from an unclean shutdown, not a live process holding the
//      path -- but blindly unlinking would clobber unrelated files. We probe
//      with `UnixStream::connect`: live process answers => Fatal "held by
//      another live process"; failed probe only unlinks after confirming the
//      path is itself a Unix socket.
//   2. Explicit `set_nonblocking(true)` on each cloned FD. POSIX dup() is
//      not guaranteed to inherit O_NONBLOCK across libc implementations,
//      so we set it explicitly on every clone before handing to tokio.
//   3. `try_get` is `async fn` even though the body has no await points.
//      This keeps the API symmetric with `UnixConnect::connect` (which is
//      necessarily async) so callers always write `.await?`. The compiler
//      inlines no-await async fns -- zero runtime cost, room to add metric /
//      tracing instrumentation later without breaking the API.
//   4. Single-file `#[cfg(unix)]` gating + `compile_error!` on non-Unix.
pub fn generate_unix_listen_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    addr: &syn::LitStr,
    env: Option<&syn::LitStr>,
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
    // Why probe-then-unlink rather than blind unlink: see the docstring above
    // (point 1). A failed probe is only stale after the path is confirmed to be
    // a Unix socket; ordinary files and other path types are preserved.
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
                            // Structured warn so operators investigating "who deleted
                            // my socket file" have a framework-side breadcrumb.
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
    // The kinds are listed explicitly (no catch-all) so a future ErrorKind
    // addition forces the maintainer to make a deliberate choice.
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

    // Framework path: init_fallible wraps the closure with backoff, total
    // timeout, and cancellation -- we just supply the failable operation.
    let framework_init_fn = quote! {
        service_daemon::__private::init_fallible(
            #struct_name_str,
            policy,
            cancel,
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
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
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
            // async fn even though body is sync: keep API symmetric with
            // UnixConnect::connect (which is necessarily async). No await
            // points -> compiler inlines, zero runtime cost. Future metric /
            // tracing instrumentation can be added without breaking the API.
            //
            // We explicitly set_nonblocking(true) on the cloned FD because
            // POSIX dup() is not guaranteed to inherit O_NONBLOCK across libc
            // implementations -- relying on inheritance is
            // undefined-behavior-adjacent on macOS and FreeBSD.
            pub async fn try_get(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixListener> {
                let cloned = self.0.try_clone()?;
                cloned.set_nonblocking(true)?;
                service_daemon::__private::tokio::net::UnixListener::from_std(cloned)
            }

            /// Accept one connection from the configured Unix socket.
            pub async fn accept(&self) -> std::io::Result<(
                service_daemon::__private::tokio::net::UnixStream,
                service_daemon::__private::tokio::net::unix::SocketAddr,
            )> {
                self.try_get().await?.accept().await
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
// Why hold path, not stream: UnixStream is a stateful kernel resource. A
// `try_clone` would let multiple callers consume bytes from the same kernel
// buffer, breaking any read-side framing. Each `try_connect` therefore opens
// a new independent stream. The framework intentionally does NOT pool because
// UDS connections are local and cheap to recreate; pooling would impose a
// semantic ("which clone am I sharing?") that callers don't want.
//
// Why probe at init: provider initialization runs `connect()` once and
// immediately drops the stream. Two purposes:
//   1. With eager = true, this blocks the system startup wave until the peer
//      is reachable. Adapter-style daemons routinely depend on a sidecar /
//      supervisor that must be up before our own services start. The
//      init_fallible backoff lets us tolerate the peer starting slightly
//      after us.
//   2. Fail-fast on misconfiguration: a typo in the path becomes Fatal at
//      init time, not at first connect() somewhere in the hot path.
// Peer servers WILL observe an accept() followed by an instant close --
// this is normal and any reasonable server design handles port-scanner /
// health-probe traffic the same way.
pub fn generate_unix_connect_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    addr: &syn::LitStr,
    env: Option<&syn::LitStr>,
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

    // ConnectionRefused / NotFound / ConnectionAborted: peer is starting up.
    // Retryable.
    //   - ConnectionRefused: peer hasn't called accept() yet
    //   - NotFound: peer hasn't created the socket file yet
    //   - ConnectionAborted: peer accepted but immediately closed (init race)
    // PermissionDenied: EACCES on the path -- a permissions issue is not a
    // transient state, the operator has to fix it. Fatal.
    let probe_and_classify = quote! {
        // One-shot probe stream is created and immediately dropped. The
        // sole purpose is reachability validation; we do not store it.
        let _probe = service_daemon::__private::tokio::net::UnixStream::connect(&path)
            .await
            .map_err(|e| {
                let msg = format!(
                    "Provider '{}' failed to probe Unix socket '{}': {}",
                    #struct_name_str, path, e
                );
                match e.kind() {
                    std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut => {
                        service_daemon::ProviderError::Retryable(msg)
                    }
                    _ => service_daemon::ProviderError::Fatal(msg),
                }
            })?;
        drop(_probe);
    };

    // Framework path: init_fallible provides retry/backoff/timeout. The
    // returned Arc<Self> caches only the path; subsequent try_connect()
    // calls open fresh streams.
    let framework_init_fn = quote! {
        service_daemon::__private::init_fallible(
            #struct_name_str,
            policy,
            cancel,
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
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
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
                #probe_and_classify
                Ok(Self {
                    path: std::sync::Arc::new(std::path::PathBuf::from(path)),
                })
            }

            /// Open a fresh connection to the configured Unix socket.
            ///
            /// Each call establishes an independent `tokio::net::UnixStream`.
            /// Callers needing a long-lived connection should hold the
            /// returned stream themselves; the framework intentionally does
            /// not pool because UDS connections are local and cheap.
            pub async fn try_connect(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixStream> {
                service_daemon::__private::tokio::net::UnixStream::connect(&*self.path).await
            }

            /// Open a fresh connection to the configured Unix socket.
            pub async fn connect(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixStream> {
                self.try_connect().await
            }

            /// Returns the configured socket path.
            pub fn path(&self) -> &std::path::Path {
                &self.path
            }
        }
    };

    TokenStream::from(expanded)
}

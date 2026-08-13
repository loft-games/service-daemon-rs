//! Cross-platform local IPC provider templates.

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::super::impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};
use super::super::parser::StringTemplateArg;
use super::context::has_clone_derive;

const ERROR_PIPE_BUSY: i32 = 231;

fn logical_name_expr(
    name: &StringTemplateArg,
    env: Option<&StringTemplateArg>,
) -> proc_macro2::TokenStream {
    let fallback = name.to_owned_expr();
    if let Some(env_arg) = env {
        let env_expr = env_arg.to_static_str_expr();
        quote! {
            std::env::var(#env_expr).unwrap_or_else(|_| #fallback)
        }
    } else {
        fallback
    }
}

fn unix_socket_path_expr(struct_name_str: &str) -> proc_macro2::TokenStream {
    quote! {
        fn unix_socket_path(name: &str) -> std::result::Result<std::path::PathBuf, service_daemon::ProviderError> {
            let base_dir = std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let dir = base_dir.join("service-daemon-rs");
            std::fs::create_dir_all(&dir).map_err(|error| {
                service_daemon::ProviderError::Fatal(format!(
                    "Provider '{}' failed to create local IPC runtime directory '{}': {}",
                    #struct_name_str,
                    dir.display(),
                    error
                ))
            })?;
            Ok(dir.join(format!("{name}.sock")))
        }
    }
}

fn validate_logical_name_expr(struct_name_str: &str) -> proc_macro2::TokenStream {
    quote! {
        fn validate_logical_name(name: &str) -> std::result::Result<(), service_daemon::ProviderError> {
            if name.is_empty()
                || !name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                })
            {
                return Err(service_daemon::ProviderError::Fatal(format!(
                    "Provider '{}' requires a non-empty local IPC logical name containing only ASCII letters, digits, '.', '_', and '-', got '{}'",
                    #struct_name_str,
                    name
                )));
            }
            Ok(())
        }
    }
}

/// Generates a `LocalIpcListen` provider.
pub(in crate::provider) fn generate_local_ipc_listen_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    name: &StringTemplateArg,
    env: Option<&StringTemplateArg>,
    eager: bool,
) -> TokenStream {
    let struct_name_str = struct_name.to_string();
    let clone_derive = if has_clone_derive(attrs) {
        quote! {}
    } else {
        quote! { #[derive(Clone)] }
    };
    let name_expr = logical_name_expr(name, env);
    let validate_logical_name = validate_logical_name_expr(&struct_name_str);
    let unix_socket_path = unix_socket_path_expr(&struct_name_str);
    let state_name = format_ident!("__{}LocalIpcListenState", struct_name);

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );
    let type_tokens = quote! { #struct_name };

    let unix_framework_init_fn = quote! {
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
    let unix_managed_init_fn = quote! {
        #struct_name::try_new().map(std::sync::Arc::new)
    };
    let windows_framework_init_fn = unix_framework_init_fn.clone();
    let windows_managed_init_fn = unix_managed_init_fn.clone();

    let unix_cfg = [quote! { #[cfg(unix)] }];
    let windows_cfg = [quote! { #[cfg(windows)] }];
    let unix_provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &unix_cfg,
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &unix_framework_init_fn,
        managed_init_fn: &unix_managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(LocalIpcListen)] struct {struct_name}"),
    });
    let windows_provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &windows_cfg,
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &windows_framework_init_fn,
        managed_init_fn: &windows_managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(LocalIpcListen)] struct {struct_name}"),
    });

    let expanded = quote! {
        #[cfg(not(any(unix, windows)))]
        const _: () = {
            ::std::compile_error!("`LocalIpcListen` provider template requires Unix or Windows target support");
        };

        #[cfg(unix)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<String>,
            listener: std::sync::Arc<std::os::unix::net::UnixListener>,
        }

        #[cfg(unix)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #[cfg(unix)]
        #unix_provided_impl

        #[cfg(unix)]
        impl #struct_name {
            fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_logical_name(&name)?;
                let path = Self::unix_socket_path(&name)?;
                let p = path.as_path();
                match std::fs::symlink_metadata(p) {
                    Ok(_) => {
                        match std::os::unix::net::UnixStream::connect(p) {
                            Ok(_probe_stream) => {
                                return Err(service_daemon::ProviderError::Fatal(format!(
                                    "Provider '{}': local IPC socket '{}' is held by another live process; refusing to bind",
                                    #struct_name_str, path.display(),
                                )));
                            }
                            Err(_probe_err) => {
                                let should_remove_stale_socket = match std::fs::symlink_metadata(p) {
                                    Ok(metadata) => {
                                        let file_type = metadata.file_type();
                                        if !std::os::unix::fs::FileTypeExt::is_socket(&file_type) {
                                            return Err(service_daemon::ProviderError::Fatal(format!(
                                                "Provider '{}': local IPC path '{}' exists but is not a Unix socket; refusing to remove",
                                                #struct_name_str, path.display(),
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
                                            "Provider '{}': failed to inspect existing local IPC socket path '{}': {} (kind={:?})",
                                            #struct_name_str, path.display(), metadata_err, metadata_err.kind(),
                                        )));
                                    }
                                };

                                if should_remove_stale_socket {
                                    if let Err(remove_err) = std::fs::remove_file(p) {
                                        if remove_err.kind() != std::io::ErrorKind::NotFound {
                                            return Err(service_daemon::ProviderError::Fatal(format!(
                                                "Provider '{}': failed to remove stale local IPC socket '{}': {} (kind={:?})",
                                                #struct_name_str, path.display(), remove_err, remove_err.kind(),
                                            )));
                                        }
                                    }
                                    ::tracing::warn!(
                                        provider = #struct_name_str,
                                        logical_name = %name,
                                        path = %path.display(),
                                        "Removed stale local IPC Unix socket file before binding"
                                    );
                                }
                            }
                        }
                    }
                    Err(metadata_err) if metadata_err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(metadata_err) => {
                        return Err(service_daemon::ProviderError::Fatal(format!(
                            "Provider '{}': failed to inspect existing local IPC socket path '{}': {} (kind={:?})",
                            #struct_name_str, path.display(), metadata_err, metadata_err.kind(),
                        )));
                    }
                }
                let listener = std::os::unix::net::UnixListener::bind(&path).map_err(|e| {
                    let msg = format!(
                        "Provider '{}' failed to bind local IPC Unix socket '{}' for logical name '{}': {}",
                        #struct_name_str, path.display(), name, e
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
                        "Provider '{}' failed to set nonblocking for local IPC Unix socket '{}': {}",
                        #struct_name_str, path.display(), e
                    ))
                })?;
                Ok(Self {
                    name: std::sync::Arc::new(name),
                    listener: std::sync::Arc::new(listener),
                })
            }

            #validate_logical_name
            #unix_socket_path

            fn get_listener(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixListener> {
                let cloned = self.listener.try_clone()?;
                cloned.set_nonblocking(true)?;
                service_daemon::__private::tokio::net::UnixListener::from_std(cloned)
            }

            async fn accept_raw(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixStream> {
                let (stream, _) = self.get_listener()?.accept().await?;
                Ok(stream)
            }

            /// Accept one connection as a platform-neutral local IPC stream.
            pub async fn accept(&self) -> std::io::Result<service_daemon::IpcStream> {
                self.accept_raw().await.map(service_daemon::IpcStream::Unix)
            }

            /// Returns the configured local IPC logical name.
            pub fn name(&self) -> &str {
                &self.name
            }
        }

        #[cfg(windows)]
        struct #state_name {
            initial_server: service_daemon::__private::tokio::sync::Mutex<Option<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
            >>,
            accepted_rx: service_daemon::__private::tokio::sync::Mutex<Option<
                service_daemon::__private::tokio::sync::mpsc::Receiver<
                    service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
                >,
            >>,
        }

        #[cfg(windows)]
        impl std::fmt::Debug for #state_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!(#state_name)).finish_non_exhaustive()
            }
        }

        #[cfg(windows)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<String>,
            pipe_name: std::sync::Arc<String>,
            state: std::sync::Arc<#state_name>,
            max_instances: Option<usize>,
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #[cfg(windows)]
        #windows_provided_impl

        #[cfg(windows)]
        impl #struct_name {
            fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_logical_name(&name)?;
                let pipe_name = Self::pipe_name(&name);
                let first_server = Self::create_server_instance(&pipe_name, true).map_err(|error| {
                    Self::classify_server_create_error(&name, &pipe_name, true, error)
                })?;
                Ok(Self {
                    name: std::sync::Arc::new(name),
                    pipe_name: std::sync::Arc::new(pipe_name),
                    state: std::sync::Arc::new(#state_name {
                        initial_server: service_daemon::__private::tokio::sync::Mutex::new(Some(first_server)),
                        accepted_rx: service_daemon::__private::tokio::sync::Mutex::new(None),
                    }),
                    max_instances: None,
                })
            }

            #validate_logical_name

            fn pipe_name(name: &str) -> String {
                format!(r"\\.\pipe\service-daemon-rs-{name}")
            }

            fn create_server_instance(
                pipe_name: &str,
                is_first_instance: bool,
            ) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
            > {
                Self::create_server_instance_with_max_instances(pipe_name, is_first_instance, None)
            }

            fn create_server_instance_with_max_instances(
                pipe_name: &str,
                is_first_instance: bool,
                max_instances: Option<usize>,
            ) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
            > {
                let mut options = service_daemon::__private::tokio::net::windows::named_pipe::ServerOptions::new();
                options.reject_remote_clients(true);
                options.first_pipe_instance(is_first_instance);
                if let Some(max_instances) = max_instances {
                    options.max_instances(max_instances);
                }
                options.create(pipe_name)
            }

            fn classify_server_create_error(
                name: &str,
                pipe_name: &str,
                is_first_instance: bool,
                error: std::io::Error,
            ) -> service_daemon::ProviderError {
                let msg = if is_first_instance {
                    format!(
                        "Provider '{}' failed to create first local IPC named pipe server instance '{}' for logical name '{}': {}",
                        #struct_name_str, pipe_name, name, error
                    )
                } else {
                    format!(
                        "Provider '{}' failed to create next local IPC named pipe server instance '{}' for logical name '{}': {}",
                        #struct_name_str, pipe_name, name, error
                    )
                };
                match error.kind() {
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::TimedOut => {
                        service_daemon::ProviderError::Retryable(msg)
                    }
                    _ => service_daemon::ProviderError::Fatal(msg),
                }
            }

            async fn start_accept_manager_if_needed(&self) -> std::io::Result<()> {
                let mut accepted_rx = self.state.accepted_rx.lock().await;
                if accepted_rx.is_some() {
                    return Ok(());
                }

                let first_server = {
                    let mut initial_server = self.state.initial_server.lock().await;
                    initial_server.take().ok_or_else(|| {
                        std::io::Error::other("local IPC named pipe listener lost its initial server instance")
                    })?
                };

                let (accepted_tx, receiver) = service_daemon::__private::tokio::sync::mpsc::channel(1);
                *accepted_rx = Some(receiver);

                let name = std::sync::Arc::clone(&self.name);
                let pipe_name = std::sync::Arc::clone(&self.pipe_name);
                let max_instances = self.max_instances;
                service_daemon::__private::tokio::spawn(async move {
                    Self::run_accept_manager(name, pipe_name, first_server, max_instances, accepted_tx).await;
                });

                Ok(())
            }

            async fn run_accept_manager(
                name: std::sync::Arc<String>,
                pipe_name: std::sync::Arc<String>,
                mut pending_server: service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
                max_instances: Option<usize>,
                accepted_tx: service_daemon::__private::tokio::sync::mpsc::Sender<
                    service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
                >,
            ) {
                loop {
                    let connect_result = service_daemon::__private::tokio::select! {
                        result = pending_server.connect() => result,
                        () = accepted_tx.closed() => return,
                    };

                    if connect_result.is_err() {
                        match Self::create_next_server_with_retry(
                            name.as_str(),
                            pipe_name.as_str(),
                            max_instances,
                            &accepted_tx,
                        )
                        .await
                        {
                            Some(next_server) => {
                                pending_server = next_server;
                                continue;
                            }
                            None => return,
                        }
                    }

                    if accepted_tx.send(pending_server).await.is_err() {
                        return;
                    }

                    pending_server = match Self::create_next_server_with_retry(
                        name.as_str(),
                        pipe_name.as_str(),
                        max_instances,
                        &accepted_tx,
                    )
                    .await
                    {
                        Some(next_server) => next_server,
                        None => return,
                    };
                }
            }

            async fn create_next_server_with_retry(
                _name: &str,
                pipe_name: &str,
                max_instances: Option<usize>,
                accepted_tx: &service_daemon::__private::tokio::sync::mpsc::Sender<
                    service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
                >,
            ) -> Option<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer> {
                let mut delay = std::time::Duration::from_millis(10);
                let max_delay = std::time::Duration::from_millis(250);

                loop {
                    match Self::create_server_instance_with_max_instances(pipe_name, false, max_instances) {
                        Ok(server) => return Some(server),
                        Err(_) if accepted_tx.is_closed() => return None,
                        Err(_) => {
                            service_daemon::__private::tokio::select! {
                                () = accepted_tx.closed() => return None,
                                () = service_daemon::__private::tokio::time::sleep(delay) => {}
                            }
                            delay = std::cmp::min(delay.saturating_mul(2), max_delay);
                        }
                    }
                }
            }

            async fn accept_raw(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer> {
                self.start_accept_manager_if_needed().await?;

                let mut accepted_rx = self.state.accepted_rx.lock().await;
                let receiver = accepted_rx.as_mut().ok_or_else(|| {
                    std::io::Error::other("local IPC named pipe listener manager did not start")
                })?;

                receiver.recv().await.ok_or_else(|| {
                    std::io::Error::other("local IPC named pipe listener manager stopped")
                })
            }

            /// Accept one connection as a platform-neutral local IPC stream.
            pub async fn accept(&self) -> std::io::Result<service_daemon::IpcStream> {
                self.accept_raw().await.map(service_daemon::IpcStream::NamedPipeServer)
            }

            /// Returns the configured local IPC logical name.
            pub fn name(&self) -> &str {
                &self.name
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a `LocalIpcConnect` provider.
pub(in crate::provider) fn generate_local_ipc_connect_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    name: &StringTemplateArg,
    env: Option<&StringTemplateArg>,
    eager: bool,
) -> TokenStream {
    let struct_name_str = struct_name.to_string();
    let clone_derive = if has_clone_derive(attrs) {
        quote! {}
    } else {
        quote! { #[derive(Clone)] }
    };
    let name_expr = logical_name_expr(name, env);
    let validate_logical_name = validate_logical_name_expr(&struct_name_str);
    let unix_socket_path = unix_socket_path_expr(&struct_name_str);

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
            move || async move { #struct_name::try_new().await },
        )
        .await
    };
    let managed_init_fn = quote! {
        #struct_name::try_new().await.map(std::sync::Arc::new)
    };
    let unix_cfg = [quote! { #[cfg(unix)] }];
    let windows_cfg = [quote! { #[cfg(windows)] }];
    let unix_provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &unix_cfg,
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(LocalIpcConnect)] struct {struct_name}"),
    });
    let windows_provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        item_attrs: &windows_cfg,
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(LocalIpcConnect)] struct {struct_name}"),
    });

    let expanded = quote! {
        #[cfg(not(any(unix, windows)))]
        const _: () = {
            ::std::compile_error!("`LocalIpcConnect` provider template requires Unix or Windows target support");
        };

        #[cfg(unix)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<String>,
            path: std::sync::Arc<std::path::PathBuf>,
        }

        #[cfg(unix)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #[cfg(unix)]
        #unix_provided_impl

        #[cfg(unix)]
        impl #struct_name {
            async fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_logical_name(&name)?;
                let path = Self::unix_socket_path(&name)?;
                Ok(Self {
                    name: std::sync::Arc::new(name),
                    path: std::sync::Arc::new(path),
                })
            }

            #validate_logical_name
            #unix_socket_path

            async fn connect_raw(&self) -> std::io::Result<service_daemon::__private::tokio::net::UnixStream> {
                service_daemon::__private::tokio::net::UnixStream::connect(&*self.path).await
            }

            /// Open a fresh connection as a platform-neutral local IPC stream.
            pub async fn connect(&self) -> std::io::Result<service_daemon::IpcStream> {
                self.connect_raw().await.map(service_daemon::IpcStream::Unix)
            }

            /// Returns the configured local IPC logical name.
            pub fn name(&self) -> &str {
                &self.name
            }
        }

        #[cfg(windows)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<String>,
            pipe_name: std::sync::Arc<String>,
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #[cfg(windows)]
        #windows_provided_impl

        #[cfg(windows)]
        impl #struct_name {
            async fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_logical_name(&name)?;
                let pipe_name = Self::pipe_name(&name);
                Ok(Self {
                    name: std::sync::Arc::new(name),
                    pipe_name: std::sync::Arc::new(pipe_name),
                })
            }

            #validate_logical_name

            fn pipe_name(name: &str) -> String {
                format!(r"\\.\pipe\service-daemon-rs-{name}")
            }

            fn open_client(
                pipe_name: &str,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                service_daemon::__private::tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(pipe_name)
            }

            async fn open_client_with_busy_retry(
                pipe_name: &str,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match Self::open_client(pipe_name) {
                        Ok(client) => return Ok(client),
                        Err(error)
                            if error.raw_os_error() == Some(#ERROR_PIPE_BUSY)
                                && std::time::Instant::now() < deadline =>
                        {
                            service_daemon::__private::tokio::time::sleep(
                                std::time::Duration::from_millis(10),
                            )
                            .await;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }

            async fn connect_raw(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                Self::open_client_with_busy_retry(&self.pipe_name).await
            }

            /// Open a fresh connection as a platform-neutral local IPC stream.
            pub async fn connect(&self) -> std::io::Result<service_daemon::IpcStream> {
                self.connect_raw()
                    .await
                    .map(service_daemon::IpcStream::NamedPipeClient)
            }

            /// Returns the configured local IPC logical name.
            pub fn name(&self) -> &str {
                &self.name
            }
        }
    };

    TokenStream::from(expanded)
}

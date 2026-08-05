//! Windows named pipe provider templates.

use proc_macro::TokenStream;
use quote::{format_ident, quote};

use super::super::impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};
use super::context::has_clone_derive;

const ERROR_PIPE_BUSY: i32 = 231;

fn pipe_name_expr(addr: &syn::LitStr, env: Option<&syn::LitStr>) -> proc_macro2::TokenStream {
    if let Some(env_lit) = env {
        let env_str = env_lit.value();
        quote! {
            std::env::var(#env_str).unwrap_or_else(|_| #addr.to_owned())
        }
    } else {
        quote! { #addr.to_owned() }
    }
}

fn windows_only_compile_error_guard(template_name: &str) -> proc_macro2::TokenStream {
    let message = windows_only_compile_error_message(template_name);
    quote! {
        #[cfg(not(windows))]
        const _: () = {
            ::std::compile_error!(#message);
        };
    }
}

fn windows_only_compile_error_message(template_name: &str) -> String {
    format!(
        "`{}` provider template is only available on Windows targets. \
         Wrap the `#[provider({}(r\"\\\\.\\pipe\\...\"))]` declaration in `#[cfg(windows)]`, \
         or use the Unix-specific `UnixListen` / `UnixConnect` templates on Unix targets.",
        template_name, template_name
    )
}

/// Generates a `NamedPipeListen` Windows named pipe server provider.
pub(in crate::provider) fn generate_named_pipe_listen_template(
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

    let name_expr = pipe_name_expr(addr, env);
    let compile_error_guard = windows_only_compile_error_guard("NamedPipeListen");
    let state_name = format_ident!("__{}NamedPipeListenState", struct_name);

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );
    let type_tokens = quote! { #struct_name };
    let windows_cfg = [quote! { #[cfg(windows)] }];

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
        item_attrs: &windows_cfg,
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(NamedPipeListen)] struct {struct_name}"),
    });

    let expanded = quote! {
        #compile_error_guard

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
            state: std::sync::Arc<#state_name>,
            max_instances: Option<usize>,
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #provided_impl

        #[cfg(windows)]
        impl #struct_name {
            pub fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_local_name(&name)?;
                let first_server = Self::create_server_instance(&name, true).map_err(|error| {
                    Self::classify_server_create_error(&name, true, error)
                })?;
                Ok(Self {
                    name: std::sync::Arc::new(name),
                    state: std::sync::Arc::new(#state_name {
                        initial_server: service_daemon::__private::tokio::sync::Mutex::new(Some(first_server)),
                        accepted_rx: service_daemon::__private::tokio::sync::Mutex::new(None),
                    }),
                    max_instances: None,
                })
            }

            fn validate_local_name(name: &str) -> std::result::Result<(), service_daemon::ProviderError> {
                const LOCAL_PIPE_PREFIX: &str = "\\\\.\\pipe\\";
                match name.strip_prefix(LOCAL_PIPE_PREFIX) {
                    Some(suffix) if !suffix.is_empty() => Ok(()),
                    _ => Err(service_daemon::ProviderError::Fatal(format!(
                        "Provider '{}' requires a local Windows named pipe path beginning with '{}', got '{}'",
                        #struct_name_str, LOCAL_PIPE_PREFIX, name
                    ))),
                }
            }

            fn create_server_instance(
                name: &str,
                is_first_instance: bool,
            ) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
            > {
                Self::create_server_instance_with_max_instances(name, is_first_instance, None)
            }

            fn create_server_instance_with_max_instances(
                name: &str,
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
                options.create(name)
            }

            fn classify_server_create_error(
                name: &str,
                is_first_instance: bool,
                error: std::io::Error,
            ) -> service_daemon::ProviderError {
                let msg = if is_first_instance {
                    format!(
                        "Provider '{}' failed to create first Windows named pipe server instance '{}': {}",
                        #struct_name_str, name, error
                    )
                } else {
                    format!(
                        "Provider '{}' failed to create next Windows named pipe server instance '{}': {}",
                        #struct_name_str, name, error
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
                        std::io::Error::other("named pipe listener lost its initial server instance")
                    })?
                };

                let (accepted_tx, receiver) = service_daemon::__private::tokio::sync::mpsc::channel(1);
                *accepted_rx = Some(receiver);

                let name = std::sync::Arc::clone(&self.name);
                let max_instances = self.max_instances;
                service_daemon::__private::tokio::spawn(async move {
                    Self::run_accept_manager(name, first_server, max_instances, accepted_tx).await;
                });

                Ok(())
            }

            async fn run_accept_manager(
                name: std::sync::Arc<String>,
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
                name: &str,
                max_instances: Option<usize>,
                accepted_tx: &service_daemon::__private::tokio::sync::mpsc::Sender<
                    service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
                >,
            ) -> Option<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer> {
                let mut delay = std::time::Duration::from_millis(10);
                let max_delay = std::time::Duration::from_millis(250);

                loop {
                    match Self::create_server_instance_with_max_instances(name, false, max_instances) {
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

            /// Accept one client connection and return the connected server end.
            ///
            /// A background listener manager owns the pending server instance and
            /// replenishes it after each connection. Runtime failures while creating the
            /// next pending instance are retried inside that manager, so a successful
            /// `accept()` only means a connected server end is ready for business logic.
            pub async fn accept(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer> {
                self.start_accept_manager_if_needed().await?;

                let mut accepted_rx = self.state.accepted_rx.lock().await;
                let receiver = accepted_rx.as_mut().ok_or_else(|| {
                    std::io::Error::other("named pipe listener manager did not start")
                })?;

                receiver.recv().await.ok_or_else(|| {
                    std::io::Error::other("named pipe listener manager stopped")
                })
            }

            /// Returns the configured local named pipe path.
            pub fn name(&self) -> &str {
                &self.name
            }

            #[cfg(test)]
            #[allow(dead_code)]
            fn try_new_with_max_instances_for_test(
                max_instances: usize,
            ) -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_local_name(&name)?;
                let first_server = Self::create_server_instance_with_max_instances(
                    &name,
                    true,
                    Some(max_instances),
                )
                .map_err(|error| Self::classify_server_create_error(&name, true, error))?;
                Ok(Self {
                    name: std::sync::Arc::new(name),
                    state: std::sync::Arc::new(#state_name {
                        initial_server: service_daemon::__private::tokio::sync::Mutex::new(Some(first_server)),
                        accepted_rx: service_daemon::__private::tokio::sync::Mutex::new(None),
                    }),
                    max_instances: Some(max_instances),
                })
            }
        }
    };

    TokenStream::from(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_expr_uses_literal_without_env_override() {
        let addr = syn::LitStr::new(r"\\.\pipe\default", proc_macro2::Span::call_site());

        let expr = pipe_name_expr(&addr, None).to_string();

        assert_eq!(expr, r#""\\\\.\\pipe\\default" . to_owned ()"#);
    }

    #[test]
    fn pipe_name_expr_prefers_env_with_literal_fallback() {
        let addr = syn::LitStr::new(r"\\.\pipe\default", proc_macro2::Span::call_site());
        let env = syn::LitStr::new("PIPE_NAME", proc_macro2::Span::call_site());

        let expr = pipe_name_expr(&addr, Some(&env)).to_string();

        assert!(expr.contains(r#"std :: env :: var ("PIPE_NAME")"#));
        assert!(expr.contains(r#"unwrap_or_else"#));
        assert!(expr.contains(r#""\\\\.\\pipe\\default" . to_owned ()"#));
    }

    #[test]
    fn windows_only_guard_mentions_template_and_cfg_escape() {
        let message = windows_only_compile_error_message("NamedPipeListen");

        assert!(
            message.contains(
                "`NamedPipeListen` provider template is only available on Windows targets"
            )
        );
        assert!(message.contains(r#"#[cfg(windows)]"#));
        assert!(message.contains(r#"#[provider(NamedPipeListen(r"\\.\pipe\..."))]"#));
        assert!(message.contains("UnixListen"));
        assert!(message.contains("UnixConnect"));
    }
}

/// Generates a `NamedPipeConnect` Windows named pipe client provider.
pub(in crate::provider) fn generate_named_pipe_connect_template(
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

    let name_expr = pipe_name_expr(addr, env);
    let compile_error_guard = windows_only_compile_error_guard("NamedPipeConnect");

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );
    let type_tokens = quote! { #struct_name };
    let windows_cfg = [quote! { #[cfg(windows)] }];

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
        item_attrs: &windows_cfg,
        user_span: struct_name.span(),
        param_entries: &[],
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style: HelperStyle::Fallible,
        provider_origin: format!("#[provider(NamedPipeConnect)] struct {struct_name}"),
    });

    let expanded = quote! {
        #compile_error_guard

        #[cfg(windows)]
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<String>,
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #provided_impl

        #[cfg(windows)]
        impl #struct_name {
            pub async fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = #name_expr;
                Self::validate_local_name(&name)?;
                let probe = Self::open_client(&name).map_err(|error| {
                    Self::classify_client_open_error("probe", &name, error)
                })?;
                drop(probe);
                Ok(Self {
                    name: std::sync::Arc::new(name),
                })
            }

            fn validate_local_name(name: &str) -> std::result::Result<(), service_daemon::ProviderError> {
                const LOCAL_PIPE_PREFIX: &str = "\\\\.\\pipe\\";
                match name.strip_prefix(LOCAL_PIPE_PREFIX) {
                    Some(suffix) if !suffix.is_empty() => Ok(()),
                    _ => Err(service_daemon::ProviderError::Fatal(format!(
                        "Provider '{}' requires a local Windows named pipe path beginning with '{}', got '{}'",
                        #struct_name_str, LOCAL_PIPE_PREFIX, name
                    ))),
                }
            }

            fn open_client(
                name: &str,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                service_daemon::__private::tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(name)
            }

            fn classify_client_open_error(
                operation: &str,
                name: &str,
                error: std::io::Error,
            ) -> service_daemon::ProviderError {
                let msg = format!(
                    "Provider '{}' failed to {} Windows named pipe '{}': {}",
                    #struct_name_str, operation, name, error
                );
                if error.raw_os_error() == Some(#ERROR_PIPE_BUSY) {
                    return service_daemon::ProviderError::Retryable(msg);
                }
                match error.kind() {
                    std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut => service_daemon::ProviderError::Retryable(msg),
                    std::io::ErrorKind::PermissionDenied => service_daemon::ProviderError::Fatal(msg),
                    _ => service_daemon::ProviderError::Fatal(msg),
                }
            }

            /// Open a fresh client connection to the configured named pipe.
            pub async fn connect(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                Self::open_client(&self.name)
            }

            /// Returns the configured local named pipe path.
            pub fn name(&self) -> &str {
                &self.name
            }
        }
    };

    TokenStream::from(expanded)
}

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
    let message = format!(
        "`{}` provider template is only available on Windows targets. \
         Wrap the `#[provider({}(r\"\\\\.\\pipe\\...\"))]` declaration in `#[cfg(windows)]`. \
         Unix domain sockets remain available through the Unix-only `UnixListen` and `UnixConnect` templates.",
        template_name, template_name
    );
    quote! {
        #[cfg(not(windows))]
        const _: () = {
            ::std::compile_error!(#message);
        };
    }
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
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<String>,
            next_server: std::sync::Arc<service_daemon::__private::tokio::sync::Mutex<Option<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer,
            >>>,
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name)
            }
        }

        #[cfg(windows)]
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
                    next_server: std::sync::Arc::new(service_daemon::__private::tokio::sync::Mutex::new(Some(first_server))),
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
                let mut options = service_daemon::__private::tokio::net::windows::named_pipe::ServerOptions::new();
                options.reject_remote_clients(true);
                options.first_pipe_instance(is_first_instance);
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

            /// Accept one client connection and return the connected server end.
            ///
            /// The next server instance is normally created before the connected one is
            /// yielded, matching Tokio's recommended named-pipe server pattern. If that
            /// replacement create fails after a client has connected, the connected server
            /// end is still returned and the next call will retry creating the pending
            /// server instance.
            pub async fn accept(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer> {
                let mut next_server = self.next_server.lock().await;
                if next_server.is_none() {
                    *next_server = Some(Self::create_server_instance(&self.name, false)?);
                }

                let mut connected_server = next_server.take().ok_or_else(|| {
                    std::io::Error::other("named pipe listener lost its pending server instance")
                })?;

                if let Err(error) = connected_server.connect().await {
                    *next_server = Some(connected_server);
                    return Err(error);
                }

                if let Ok(replacement) = Self::create_server_instance(&self.name, false) {
                    *next_server = Some(replacement);
                }

                Ok(connected_server)
            }

            /// Returns the configured local named pipe path.
            pub fn name(&self) -> &str {
                &self.name
            }
        }
    };

    TokenStream::from(expanded)
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

        #[cfg(windows)]
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
                match error.kind() {
                    std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::TimedOut => service_daemon::ProviderError::Retryable(msg),
                    std::io::ErrorKind::PermissionDenied => service_daemon::ProviderError::Fatal(msg),
                    _ if error.raw_os_error() == Some(#ERROR_PIPE_BUSY) => {
                        service_daemon::ProviderError::Retryable(msg)
                    }
                    _ => service_daemon::ProviderError::Fatal(msg),
                }
            }

            /// Open a fresh client connection to the configured named pipe.
            pub async fn try_connect(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                Self::open_client(&self.name)
            }

            /// Open a fresh client connection to the configured named pipe.
            pub async fn connect(
                &self,
            ) -> std::io::Result<service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient> {
                self.try_connect().await
            }

            /// Returns the configured local named pipe path.
            pub fn name(&self) -> &str {
                &self.name
            }
        }
    };

    TokenStream::from(expanded)
}

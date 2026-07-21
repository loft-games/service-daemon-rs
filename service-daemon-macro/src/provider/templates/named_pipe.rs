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
        #(#attrs)*
        #clone_derive
        #vis struct #struct_name {
            name: std::sync::Arc<std::path::PathBuf>,
            pending: std::sync::Arc<
                service_daemon::__private::tokio::sync::Mutex<
                    std::option::Option<
                        service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer
                    >
                >
            >,
        }

        #[cfg(windows)]
        impl #struct_name {
            pub fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = std::path::PathBuf::from(#name_expr);
                let first = Self::create_server_instance(&name, true).map_err(|e| {
                    let msg = format!(
                        "Provider '{}' failed to create Windows named pipe '{}': {}",
                        #struct_name_str,
                        name.display(),
                        e
                    );
                    match e.raw_os_error() {
                        Some(#ERROR_PIPE_BUSY) => service_daemon::ProviderError::Retryable(msg),
                        _ => match e.kind() {
                            std::io::ErrorKind::AddrInUse
                            | std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::TimedOut => {
                                service_daemon::ProviderError::Retryable(msg)
                            }
                            _ => service_daemon::ProviderError::Fatal(msg),
                        },
                    }
                })?;

                Ok(Self {
                    name: std::sync::Arc::new(name),
                    pending: std::sync::Arc::new(
                        service_daemon::__private::tokio::sync::Mutex::new(Some(first)),
                    ),
                })
            }

            fn create_server_instance(
                name: &std::path::Path,
                first: bool,
            ) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer
            > {
                service_daemon::__private::tokio::net::windows::named_pipe::ServerOptions::new()
                    .first_pipe_instance(first)
                    .reject_remote_clients(true)
                    .create(name)
            }
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name.display())
            }
        }

        #provided_impl

        #[cfg(windows)]
        impl #struct_name {
            pub fn name(&self) -> &std::path::Path {
                &self.name
            }

            pub async fn try_get(&self) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer
            > {
                self.accept().await
            }

            pub async fn accept(&self) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeServer
            > {
                let mut guard = self.pending.lock().await;
                let server = match guard.take() {
                    Some(server) => server,
                    None => Self::create_server_instance(&self.name, false)?,
                };
                match Self::create_server_instance(&self.name, false) {
                    Ok(next) => {
                        *guard = Some(next);
                    }
                    Err(err) => {
                        *guard = Some(server);
                        return Err(err);
                    }
                }
                drop(guard);

                server.connect().await?;
                Ok(server)
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
            name: std::sync::Arc<std::path::PathBuf>,
        }

        #[cfg(windows)]
        impl std::fmt::Display for #struct_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.name.display())
            }
        }

        #provided_impl

        #[cfg(windows)]
        impl #struct_name {
            pub async fn try_new() -> std::result::Result<Self, service_daemon::ProviderError> {
                let name = std::path::PathBuf::from(#name_expr);
                let provider = Self {
                    name: std::sync::Arc::new(name),
                };
                let _probe = provider.try_connect().await.map_err(|e| {
                    let msg = format!(
                        "Provider '{}' failed to probe Windows named pipe '{}': {}",
                        #struct_name_str,
                        provider.name.display(),
                        e
                    );
                    match e.raw_os_error() {
                        Some(#ERROR_PIPE_BUSY) => service_daemon::ProviderError::Retryable(msg),
                        _ => match e.kind() {
                            std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::TimedOut => {
                                service_daemon::ProviderError::Retryable(msg)
                            }
                            _ => service_daemon::ProviderError::Fatal(msg),
                        },
                    }
                })?;
                drop(_probe);
                Ok(provider)
            }

            pub fn name(&self) -> &std::path::Path {
                &self.name
            }

            pub async fn try_connect(&self) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient
            > {
                service_daemon::__private::tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(&*self.name)
            }

            pub async fn connect(&self) -> std::io::Result<
                service_daemon::__private::tokio::net::windows::named_pipe::NamedPipeClient
            > {
                self.try_connect().await
            }
        }
    };

    TokenStream::from(expanded)
}

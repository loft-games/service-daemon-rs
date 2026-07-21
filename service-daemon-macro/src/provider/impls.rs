//! Shared provider capability impl generation.

use quote::{format_ident, quote, quote_spanned};

/// Return shape for generated provider helpers.
#[derive(Clone, Copy)]
pub(super) enum HelperStyle {
    Infallible,
    Fallible,
}

pub(super) struct ProvidedImplConfig<'a> {
    pub type_tokens: &'a proc_macro2::TokenStream,
    pub singleton_name: &'a syn::Ident,
    pub item_attrs: &'a [proc_macro2::TokenStream],
    pub user_span: proc_macro2::Span,
    pub param_entries: &'a [proc_macro2::TokenStream],
    pub eager: bool,
    pub framework_init_fn: &'a proc_macro2::TokenStream,
    pub managed_init_fn: &'a proc_macro2::TokenStream,
    pub helper_style: HelperStyle,
    pub provider_origin: String,
}

/// Generates the provider capability trait impls and convenience methods
/// for a provider type, and registers a `ProviderEntry` in the
/// `PROVIDER_REGISTRY` for dependency graph analysis.
pub(super) fn generate_provided_impl(config: ProvidedImplConfig<'_>) -> proc_macro2::TokenStream {
    let ProvidedImplConfig {
        type_tokens,
        singleton_name,
        item_attrs,
        user_span,
        param_entries,
        eager,
        framework_init_fn,
        managed_init_fn,
        helper_style,
        provider_origin,
    } = config;
    // Use quote_spanned! so that if the type is missing Clone/Send/Sync,
    // the compiler error points to the user's struct definition or fn return
    // type rather than an opaque macro expansion site.
    let bounds_assertion = quote_spanned! { user_span =>
        const _: () = {
            fn __assert_provider_bounds<T: Clone + Send + Sync + 'static>() {}
            fn __check() { __assert_provider_bounds::<#type_tokens>(); }
        };
    };

    let watchable_impl = quote! {
        #(#item_attrs)*
        impl service_daemon::WatchableProvided for #type_tokens {
            fn watch_dependency() -> service_daemon::ProviderDependencyWatch {
                service_daemon::__private::provider_dependency_watch(&#singleton_name)
            }
        }
    };

    // Generate a unique entry name for the PROVIDER_REGISTRY slice.
    let type_name_str = quote!(#type_tokens).to_string().replace(' ', "");
    let entry_name = format_ident!(
        "__PROVIDER_ENTRY_{}",
        type_name_str
            .to_uppercase()
            .replace(|c: char| !c.is_alphanumeric(), "_")
    );

    let init_fn_name = format_ident!(
        "__PROVIDER_INIT_{}",
        type_name_str
            .to_uppercase()
            .replace(|c: char| !c.is_alphanumeric(), "_")
    );

    let provider_origin_lit = syn::LitStr::new(&provider_origin, user_span);
    let provider_definition_site = quote_spanned! { user_span =>
        (file!(), line!(), column!())
    };
    let invariant_panic = |helper_name: &'static str| {
        quote! {
            |error| {
                panic!(
                    "service-daemon provider `{}` failed in direct helper `{}`. provider_origin={}, provider_defined_at={}:{}:{}, helper_called_at={}:{}:{}, module={}, error={}. Direct helpers are generated only for providers with no declared fallible initialization path; use `Result<T, service_daemon::ProviderError>` if initialization can fail.",
                    #type_name_str,
                    #helper_name,
                    #provider_origin_lit,
                    provider_definition_site.0,
                    provider_definition_site.1,
                    provider_definition_site.2,
                    helper_callsite.file(),
                    helper_callsite.line(),
                    helper_callsite.column(),
                    module_path!(),
                    error,
                )
            }
        }
    };

    let resolve_panic = invariant_panic("resolve");
    let resolve_rwlock_panic = invariant_panic("resolve_rwlock");
    let resolve_mutex_panic = invariant_panic("resolve_mutex");

    let helper_impl = match helper_style {
        HelperStyle::Infallible => {
            quote! {
                #(#item_attrs)*
                impl #type_tokens {
                    /// Resolves an immutable snapshot for this provider.
                    #[track_caller]
                    pub fn resolve() -> impl std::future::Future<Output = std::sync::Arc<Self>> + Send {
                        let provider_definition_site = #provider_definition_site;
                        let helper_callsite = std::panic::Location::caller();
                        async move {
                            <Self as service_daemon::Provided>::resolve()
                                .await
                                .unwrap_or_else(#resolve_panic)
                        }
                    }

                    /// Resolves a tracked RwLock for this provider.
                    #[track_caller]
                    pub fn resolve_rwlock() -> impl std::future::Future<Output = std::sync::Arc<service_daemon::RwLock<Self>>> + Send {
                        let provider_definition_site = #provider_definition_site;
                        let helper_callsite = std::panic::Location::caller();
                        async move {
                            <Self as service_daemon::ManagedProvided>::resolve_rwlock()
                                .await
                                .unwrap_or_else(#resolve_rwlock_panic)
                        }
                    }

                    /// Resolves a tracked Mutex for this provider.
                    #[track_caller]
                    pub fn resolve_mutex() -> impl std::future::Future<Output = std::sync::Arc<service_daemon::Mutex<Self>>> + Send {
                        let provider_definition_site = #provider_definition_site;
                        let helper_callsite = std::panic::Location::caller();
                        async move {
                            <Self as service_daemon::ManagedProvided>::resolve_mutex()
                                .await
                                .unwrap_or_else(#resolve_mutex_panic)
                        }
                    }

                    /// Resolves the raw managed result for this provider.
                    pub async fn resolve_managed() -> std::result::Result<std::sync::Arc<Self>, service_daemon::ProviderError> {
                        <Self as service_daemon::ManagedProvided>::resolve_managed().await
                    }
                }
            }
        }
        HelperStyle::Fallible => {
            quote! {
                #(#item_attrs)*
                impl #type_tokens {
                    /// Resolves an immutable snapshot for this provider.
                    pub async fn resolve() -> std::result::Result<std::sync::Arc<Self>, service_daemon::ProviderInitError> {
                        <Self as service_daemon::Provided>::resolve().await
                    }

                    /// Resolves a tracked RwLock for this provider.
                    pub async fn resolve_rwlock() -> std::result::Result<std::sync::Arc<service_daemon::RwLock<Self>>, service_daemon::ProviderInitError> {
                        <Self as service_daemon::ManagedProvided>::resolve_rwlock().await
                    }

                    /// Resolves a tracked Mutex for this provider.
                    pub async fn resolve_mutex() -> std::result::Result<std::sync::Arc<service_daemon::Mutex<Self>>, service_daemon::ProviderInitError> {
                        <Self as service_daemon::ManagedProvided>::resolve_mutex().await
                    }

                    /// Resolves the raw managed result for this provider.
                    pub async fn resolve_managed() -> std::result::Result<std::sync::Arc<Self>, service_daemon::ProviderError> {
                        <Self as service_daemon::ManagedProvided>::resolve_managed().await
                    }
                }
            }
        }
    };
    quote! {
        #(#item_attrs)*
        #bounds_assertion

        #(#item_attrs)*
        static #singleton_name: service_daemon::__private::StateManager<#type_tokens> = service_daemon::__private::StateManager::new();

        #(#item_attrs)*
        impl service_daemon::Provided for #type_tokens {
            async fn resolve() -> std::result::Result<std::sync::Arc<Self>, service_daemon::ProviderInitError> {
                service_daemon::__private::resolve_provider_snapshot(&#singleton_name, || async {
                    let policy = service_daemon::RestartPolicy::default();
                    let cancel = service_daemon::__private::current_cancellation_token();
                    let provider_init_context = service_daemon::__private::ProviderInitBoundaryContext::new(
                        #type_name_str,
                        service_daemon::__private::ProviderInitBoundaryKind::SnapshotResolve,
                    );
                    match service_daemon::__private::catch_init_panic(
                        #type_name_str,
                        async move { #framework_init_fn },
                    )
                    .await
                    {
                        Ok(result) => service_daemon::__private::provider_init_failure_boundary(provider_init_context, result),
                        Err(error) => service_daemon::__private::provider_init_failure_boundary(
                            provider_init_context,
                            Err(service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::Panic,
                                error,
                            )),
                        ),
                    }
                })
                .await
            }
        }

        #(#item_attrs)*
        impl service_daemon::ManagedProvided for #type_tokens {
            async fn resolve_rwlock() -> std::result::Result<std::sync::Arc<service_daemon::RwLock<Self>>, service_daemon::ProviderInitError> {
                service_daemon::__private::resolve_provider_rwlock(&#singleton_name, || async {
                    let policy = service_daemon::RestartPolicy::default();
                    let cancel = service_daemon::__private::current_cancellation_token();
                    let provider_init_context = service_daemon::__private::ProviderInitBoundaryContext::new(
                        #type_name_str,
                        service_daemon::__private::ProviderInitBoundaryKind::RwLockResolve,
                    );
                    match service_daemon::__private::catch_init_panic(
                        #type_name_str,
                        async move { #framework_init_fn },
                    )
                    .await
                    {
                        Ok(result) => service_daemon::__private::provider_init_failure_boundary(provider_init_context, result),
                        Err(error) => service_daemon::__private::provider_init_failure_boundary(
                            provider_init_context,
                            Err(service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::Panic,
                                error,
                            )),
                        ),
                    }
                })
                .await
            }

            async fn resolve_mutex() -> std::result::Result<std::sync::Arc<service_daemon::Mutex<Self>>, service_daemon::ProviderInitError> {
                service_daemon::__private::resolve_provider_mutex(&#singleton_name, || async {
                    let policy = service_daemon::RestartPolicy::default();
                    let cancel = service_daemon::__private::current_cancellation_token();
                    let provider_init_context = service_daemon::__private::ProviderInitBoundaryContext::new(
                        #type_name_str,
                        service_daemon::__private::ProviderInitBoundaryKind::MutexResolve,
                    );
                    match service_daemon::__private::catch_init_panic(
                        #type_name_str,
                        async move { #framework_init_fn },
                    )
                    .await
                    {
                        Ok(result) => service_daemon::__private::provider_init_failure_boundary(provider_init_context, result),
                        Err(error) => service_daemon::__private::provider_init_failure_boundary(
                            provider_init_context,
                            Err(service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::Panic,
                                error,
                            )),
                        ),
                    }
                })
                .await
            }

            async fn resolve_managed() -> std::result::Result<std::sync::Arc<Self>, service_daemon::ProviderError> {
                service_daemon::__private::resolve_provider_managed(&#singleton_name, || async {
                    let policy = service_daemon::RestartPolicy::default();
                    let cancel = service_daemon::__private::current_cancellation_token();
                    #managed_init_fn
                })
                .await
            }
        }

        #watchable_impl

        #helper_impl

        #(#item_attrs)*
        fn #init_fn_name(
            policy: service_daemon::RestartPolicy,
            cancel: service_daemon::__private::tokio_util::sync::CancellationToken,
        ) -> service_daemon::__private::futures::future::BoxFuture<'static, std::result::Result<(), service_daemon::ProviderInitError>> {
            Box::pin(async move {
                service_daemon::__private::resolve_provider_snapshot(&#singleton_name, || async {
                    let provider_init_context = service_daemon::__private::ProviderInitBoundaryContext::new(
                        #type_name_str,
                        service_daemon::__private::ProviderInitBoundaryKind::EagerInit,
                    );
                    match service_daemon::__private::catch_init_panic(
                        #type_name_str,
                        async move { #framework_init_fn },
                    )
                    .await
                    {
                        Ok(result) => service_daemon::__private::provider_init_failure_boundary(provider_init_context, result),
                        Err(error) => service_daemon::__private::provider_init_failure_boundary(
                            provider_init_context,
                            Err(service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::Panic,
                                error,
                            )),
                        ),
                    }
                })
                .await
                .map(|_| ())
            })
        }

        /// Auto-generated provider registry entry for dependency graph analysis.
        #(#item_attrs)*
        #[allow(unsafe_code)] // linkme uses #[link_section] internally
        #[service_daemon::__private::linkme::distributed_slice(service_daemon::__private::PROVIDER_REGISTRY)]
        #[linkme(crate = service_daemon::__private::linkme)]
        static #entry_name: service_daemon::__private::ProviderEntry = service_daemon::__private::ProviderEntry {
            name: #type_name_str,
            module: module_path!(),
            type_id: std::any::TypeId::of::<#type_tokens>(),
            params: &[#(#param_entries),*],
            eager: #eager,
            init: #init_fn_name,
        };
    }
}

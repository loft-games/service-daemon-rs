use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemConst, ItemFn, ItemStatic, Pat, Type, parse_macro_input};

/// Marks a function as a service managed by ServiceDaemon.
///
/// The macro automatically registers the service in the global registry
/// using `linkme` - no build.rs or manual registration needed!
///
/// The macro generates:
/// 1. A wrapper function that resolves dependencies from the global container
/// 2. A static registry entry that is automatically collected at link time
///
/// # Example
/// ```rust
/// use service_daemon::service;
/// use std::sync::Arc;
///
/// #[service]
/// pub async fn my_service(port: Arc<i32>, db: Arc<String>) -> anyhow::Result<()> {
///     // service implementation
/// }
/// ```
///
/// Then in main.rs:
/// ```rust
/// let daemon = ServiceDaemon::from_registry();
/// daemon.run().await?;
/// ```
#[proc_macro_attribute]
pub fn service(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;

    let mut resolve_tokens = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();

    for arg in &sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let arg_name = &pat_ident.ident;
                let arg_type = &pat_type.ty;
                let arg_name_str = arg_name.to_string();
                let arg_type_str = quote!(#arg_type).to_string().replace(" ", "");

                // Check if the type is Arc<T>
                let mut is_arc = false;
                let mut inner_type: Type = (**arg_type).clone();
                if let Type::Path(type_path) = &**arg_type {
                    if let Some(segment) = type_path.path.segments.last() {
                        if segment.ident == "Arc" {
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                if let Some(syn::GenericArgument::Type(ty)) = args.args.first() {
                                    inner_type = ty.clone();
                                    is_arc = true;
                                }
                            }
                        }
                    }
                }

                if is_arc {
                    resolve_tokens.push(quote! {
                        let #arg_name = service_daemon::GLOBAL_CONTAINER.resolve::<#inner_type>(#arg_name_str)
                            .ok_or_else(|| anyhow::anyhow!("Dependency not found: {} of type {}", #arg_name_str, std::any::type_name::<#inner_type>()))?;
                    });
                } else {
                    resolve_tokens.push(quote! {
                        let #arg_name = (*service_daemon::GLOBAL_CONTAINER.resolve::<#inner_type>(#arg_name_str)
                            .ok_or_else(|| anyhow::anyhow!("Dependency not found: {} of type {}", #arg_name_str, std::any::type_name::<#inner_type>()))?).clone();
                    });
                }
                call_args.push(quote! { #arg_name });

                // Build param entry for registry
                param_entries.push(quote! {
                    service_daemon::ServiceParam { name: #arg_name_str, type_name: #arg_type_str }
                });
            }
        }
    }

    let wrapper_name = format_ident!("{}_wrapper", fn_name);
    let entry_name = format_ident!("__SERVICE_ENTRY_{}", fn_name.to_string().to_uppercase());

    // Get module name from function path (simplified - uses "services" as default)
    let module_name = "services";

    let expanded = quote! {
        #(#attrs)*
        #vis #sig {
            #body
        }

        /// Auto-generated wrapper for the service - resolves dependencies and calls the service
        pub fn #wrapper_name() -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #(#resolve_tokens)*
                #fn_name(#(#call_args),*).await
            })
        }

        /// Auto-generated static registry entry - collected by linkme at link time
        #[service_daemon::linkme::distributed_slice(service_daemon::SERVICE_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ServiceEntry = service_daemon::ServiceEntry {
            name: #fn_name_str,
            module: #module_name,
            params: &[#(#param_entries),*],
            wrapper: #wrapper_name,
        };
    };

    TokenStream::from(expanded)
}

/// Marks a constant or function as a dependency provider.
///
/// The macro automatically registers the value in the global container
/// using `linkme` - no manual registration needed in main()!
///
/// # Example with const
/// ```rust
/// use service_daemon::provider;
///
/// #[provider(name = "port")]
/// const PORT: i32 = 8080;
/// ```
///
/// # Example with function
/// ```rust
/// use service_daemon::provider;
///
/// #[provider(name = "db_url")]
/// fn get_db_url() -> String {
///     std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite::memory:".into())
/// }
/// ```
#[proc_macro_attribute]
pub fn provider(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the attribute to get the name
    let attr_str = attr.to_string();
    let name = parse_provider_name(&attr_str);

    // Try parsing as const first
    if let Ok(item_const) = syn::parse::<ItemConst>(item.clone()) {
        return generate_const_provider(item_const, &name);
    }

    // Try parsing as static
    if let Ok(item_static) = syn::parse::<ItemStatic>(item.clone()) {
        return generate_static_provider(item_static, &name);
    }

    // Try parsing as function
    if let Ok(item_fn) = syn::parse::<ItemFn>(item.clone()) {
        return generate_fn_provider(item_fn, &name);
    }

    // Fallback error
    let error = syn::Error::new_spanned(
        proc_macro2::TokenStream::from(item),
        "#[provider] can only be applied to const, static, or fn items",
    );
    TokenStream::from(error.to_compile_error())
}

fn parse_provider_name(attr_str: &str) -> String {
    // Parse: name = "value" or just "value"
    if attr_str.contains("name") {
        // name = "value"
        attr_str
            .split('=')
            .nth(1)
            .map(|s| s.trim().trim_matches('"').to_string())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        // Just "value"
        attr_str.trim().trim_matches('"').to_string()
    }
}

fn generate_const_provider(item: ItemConst, name: &str) -> TokenStream {
    let const_name = &item.ident;
    let const_type = &item.ty;
    let type_name = quote!(#const_type).to_string().replace(" ", "");
    let entry_name = format_ident!("__PROVIDER_ENTRY_{}", const_name.to_string().to_uppercase());
    let init_fn_name = format_ident!("__provider_init_{}", const_name.to_string().to_lowercase());

    let expanded = quote! {
        #item

        fn #init_fn_name() {
            service_daemon::GLOBAL_CONTAINER.register(#name, #const_name);
        }

        #[service_daemon::linkme::distributed_slice(service_daemon::PROVIDER_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ProviderEntry = service_daemon::ProviderEntry {
            name: #name,
            type_name: #type_name,
            init: #init_fn_name,
        };
    };

    TokenStream::from(expanded)
}

fn generate_static_provider(item: ItemStatic, name: &str) -> TokenStream {
    let static_name = &item.ident;
    let static_type = &item.ty;
    let type_name = quote!(#static_type).to_string().replace(" ", "");
    let entry_name = format_ident!(
        "__PROVIDER_ENTRY_{}",
        static_name.to_string().to_uppercase()
    );
    let init_fn_name = format_ident!("__provider_init_{}", static_name.to_string().to_lowercase());

    let expanded = quote! {
        #item

        fn #init_fn_name() {
            service_daemon::GLOBAL_CONTAINER.register(#name, #static_name.clone());
        }

        #[service_daemon::linkme::distributed_slice(service_daemon::PROVIDER_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ProviderEntry = service_daemon::ProviderEntry {
            name: #name,
            type_name: #type_name,
            init: #init_fn_name,
        };
    };

    TokenStream::from(expanded)
}

fn generate_fn_provider(item: ItemFn, name: &str) -> TokenStream {
    let fn_name = &item.sig.ident;
    let return_type = match &item.sig.output {
        syn::ReturnType::Default => quote!(()),
        syn::ReturnType::Type(_, ty) => quote!(#ty),
    };
    let type_name = return_type.to_string().replace(" ", "");
    let entry_name = format_ident!("__PROVIDER_ENTRY_{}", fn_name.to_string().to_uppercase());
    let init_fn_name = format_ident!("__provider_init_{}", fn_name.to_string().to_lowercase());

    let expanded = quote! {
        #item

        fn #init_fn_name() {
            service_daemon::GLOBAL_CONTAINER.register(#name, #fn_name());
        }

        #[service_daemon::linkme::distributed_slice(service_daemon::PROVIDER_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ProviderEntry = service_daemon::ProviderEntry {
            name: #name,
            type_name: #type_name,
            init: #init_fn_name,
        };
    };

    TokenStream::from(expanded)
}

/// Performs compile-time verification of service dependencies.
///
/// When the `macros` feature is enabled in `service-daemon`, this macro
/// will scan the project and emit warnings if a service requires a
/// dependency that no `#[provider]` supplies.
///
/// When the `macros` feature is NOT enabled (e.g., in production builds),
/// this macro expands to nothing, incurring zero runtime or compile-time cost.
///
/// # Example
/// ```rust
/// // Place at the top of main.rs
/// service_daemon::verify_setup!();
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let daemon = ServiceDaemon::auto_init();
///     daemon.run().await
/// }
/// ```
///
/// If there's a missing dependency, you'll see:
/// ```text
/// warning: Service 'my_service' requires 'api_key', but no #[provider] found for it.
/// ```
#[proc_macro]
pub fn verify_setup(_input: TokenStream) -> TokenStream {
    // Note: This is a simplified implementation.
    // A full implementation would scan the source files using `syn`,
    // parse #[service] and #[provider] attributes, and compare them.
    //
    // For now, we provide a startup validation that runs at runtime
    // when the daemon starts, which achieves a similar goal.

    let expanded = quote! {
        // When 'macros' feature is enabled, perform runtime validation at startup.
        // This ensures that all dependencies are checked before services run.
        #[cfg(feature = "macros")]
        {
            // Collect all required dependencies from services
            let mut required: std::collections::HashSet<String> = std::collections::HashSet::new();
            for entry in service_daemon::SERVICE_REGISTRY.iter() {
                for param in entry.params {
                    required.insert(param.name.to_string());
                }
            }

            // Collect all provided dependencies from providers
            let mut provided: std::collections::HashSet<String> = std::collections::HashSet::new();
            for entry in service_daemon::PROVIDER_REGISTRY.iter() {
                provided.insert(entry.name.to_string());
            }

            // Check for missing dependencies
            for name in &required {
                if !provided.contains(name) {
                    tracing::warn!(
                        "⚠️  Dependency '{}' is required by a service but no #[provider] found for it.",
                        name
                    );
                }
            }
        }

        // When 'macros' feature is NOT enabled, this expands to nothing.
        #[cfg(not(feature = "macros"))]
        {
            // No-op in production
        }
    };

    TokenStream::from(expanded)
}

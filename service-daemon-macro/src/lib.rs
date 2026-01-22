use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemConst, ItemFn, ItemStatic, ItemStruct, Pat, Type, parse_macro_input};

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
                if let Type::Path(type_path) = &**arg_type {
                    if let Some(segment) = type_path.path.segments.last() {
                        if segment.ident == "Arc" {
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                if let Some(syn::GenericArgument::Type(inner_type)) =
                                    args.args.first()
                                {
                                    // Type-Based DI: use T::resolve() for compile-time verification
                                    resolve_tokens.push(quote! {
                                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve();
                                    });
                                    call_args.push(quote! { #arg_name });

                                    // Build param entry for registry
                                    param_entries.push(quote! {
                                        service_daemon::ServiceParam { name: #arg_name_str, type_name: #arg_type_str }
                                    });
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Non-Arc types are not supported for DI
                let error = syn::Error::new_spanned(
                    arg_type,
                    "Service parameters must be Arc<T> where T implements Provided",
                );
                return TokenStream::from(error.to_compile_error());
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

        /// Auto-generated wrapper for the service - resolves dependencies via Type-Based DI
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
    // Parse the attribute to get the name (for legacy)
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

    // Try parsing as struct
    if let Ok(item_struct) = syn::parse::<ItemStruct>(item.clone()) {
        return generate_struct_provider(item_struct, &attr_str);
    }

    // Fallback error
    let error = syn::Error::new_spanned(
        proc_macro2::TokenStream::from(item),
        "#[provider] can only be applied to const, static, fn, or struct items",
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

/// Parsed attributes from #[provider(...)]
struct ProviderAttrs {
    default_value: Option<String>,
    env_name: Option<String>,
}

/// Parses attributes from #[provider(default = ..., env_name = "...")]
fn parse_provider_attrs(attr_str: &str) -> ProviderAttrs {
    let mut attrs = ProviderAttrs {
        default_value: None,
        env_name: None,
    };

    // Handle empty attributes
    if attr_str.trim().is_empty() {
        return attrs;
    }

    // Parse key-value pairs, handling nested parentheses
    let mut depth = 0;
    let mut current_key = String::new();
    let mut current_value = String::new();
    let mut in_value = false;
    let mut in_string = false;

    for ch in attr_str.chars() {
        match ch {
            '"' if depth == 0 => {
                in_string = !in_string;
                current_value.push(ch);
            }
            '(' | '[' | '{' => {
                depth += 1;
                if in_value {
                    current_value.push(ch);
                }
            }
            ')' | ']' | '}' => {
                if depth > 0 {
                    depth -= 1;
                }
                if in_value && depth > 0 {
                    current_value.push(ch);
                }
            }
            '=' if depth == 0 && !in_string && !in_value => {
                in_value = true;
            }
            ',' if depth == 0 && !in_string => {
                // End of key-value pair
                let key = current_key.trim().to_lowercase();
                let val = current_value.trim().to_string();

                match key.as_str() {
                    "default" | "value" => attrs.default_value = Some(val),
                    "env_name" => attrs.env_name = Some(val.trim_matches('"').to_string()),
                    _ => {}
                }

                current_key.clear();
                current_value.clear();
                in_value = false;
            }
            _ => {
                if in_value {
                    current_value.push(ch);
                } else {
                    current_key.push(ch);
                }
            }
        }
    }

    // Handle last key-value pair
    if !current_key.is_empty() {
        let key = current_key.trim().to_lowercase();
        let val = current_value.trim().to_string();

        match key.as_str() {
            "default" | "value" => attrs.default_value = Some(val),
            "env_name" => attrs.env_name = Some(val.trim_matches('"').to_string()),
            _ => {}
        }
    }

    attrs
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

        // Note: impl Provided is NOT generated for function providers
        // because return types may be foreign types (orphan rule).
        // Use #[provider] on a struct for Type-Based DI.
    };

    TokenStream::from(expanded)
}

/// Generates a provider for a struct with automatic field injection.
///
/// For each field of type `Arc<T>`, it calls `T::resolve()` to inject dependencies.
/// For single-element tuple structs with `default = ...`, generates Deref, Display, Default.
fn generate_struct_provider(item: ItemStruct, attr_str: &str) -> TokenStream {
    let struct_name = &item.ident;
    let vis = &item.vis;
    let attrs = &item.attrs;
    let generics = &item.generics;
    let fields = &item.fields;
    let semi = &item.semi_token;

    // Parse attributes (default, env_name)
    let provider_attrs = parse_provider_attrs(attr_str);

    // Generate struct definition with proper syntax
    let struct_def = if semi.is_some() {
        // Tuple struct or unit struct
        quote! {
            #(#attrs)*
            #vis struct #struct_name #generics #fields;
        }
    } else {
        // Named struct
        quote! {
            #(#attrs)*
            #vis struct #struct_name #generics #fields
        }
    };

    // Check if it's a single-element tuple struct
    let is_single_tuple = matches!(fields, syn::Fields::Unnamed(f) if f.unnamed.len() == 1);

    // Get the inner type for single-element tuple structs
    let inner_type = if is_single_tuple {
        if let syn::Fields::Unnamed(f) = fields {
            Some(f.unnamed.first().unwrap().ty.clone())
        } else {
            None
        }
    } else {
        None
    };

    // Generate extra traits ONLY for single-element tuple structs
    let extra_traits = if let Some(ref inner) = inner_type {
        quote! {
            impl std::ops::Deref for #struct_name {
                type Target = #inner;
                fn deref(&self) -> &#inner {
                    &self.0
                }
            }

            impl std::fmt::Display for #struct_name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(f, "{}", self.0)
                }
            }
        }
    } else {
        quote! {}
    };

    // Generate Default impl for single-element tuple structs
    let default_impl = if let Some(ref _inner) = inner_type {
        // Build the default expression
        let default_expr = if let Some(ref env_name) = provider_attrs.env_name {
            // Use env var with fallback to default
            if let Some(ref default_val) = provider_attrs.default_value {
                let default_tokens: proc_macro2::TokenStream = default_val
                    .parse()
                    .unwrap_or_else(|_| quote! { Default::default() });
                quote! {
                    std::env::var(#env_name).unwrap_or_else(|_| #default_tokens)
                }
            } else {
                quote! {
                    std::env::var(#env_name).expect(concat!("Environment variable ", #env_name, " not set"))
                }
            }
        } else if let Some(ref default_val) = provider_attrs.default_value {
            // Just use the default value
            let default_tokens: proc_macro2::TokenStream = default_val
                .parse()
                .unwrap_or_else(|_| quote! { Default::default() });
            quote! { #default_tokens }
        } else {
            // No default specified, skip Default impl
            quote! {}
        };

        if provider_attrs.default_value.is_some() || provider_attrs.env_name.is_some() {
            quote! {
                impl Default for #struct_name {
                    fn default() -> Self {
                        Self(#default_expr)
                    }
                }
            }
        } else {
            quote! {}
        }
    } else {
        quote! {}
    };

    // Generate constructor based on field type
    let constructor = match fields {
        syn::Fields::Named(named_fields) => {
            let mut field_inits = Vec::new();
            for field in &named_fields.named {
                let field_name = field.ident.as_ref().unwrap();
                let field_type = &field.ty;

                // Check if it's Arc<T>
                if let Type::Path(type_path) = field_type {
                    if let Some(segment) = type_path.path.segments.last() {
                        if segment.ident == "Arc" {
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                if let Some(syn::GenericArgument::Type(inner_type)) =
                                    args.args.first()
                                {
                                    field_inits.push(quote! {
                                        #field_name: <#inner_type as service_daemon::Provided>::resolve()
                                    });
                                    continue;
                                }
                            }
                        }
                    }
                }

                // For non-Arc fields, use Default
                field_inits.push(quote! {
                    #field_name: Default::default()
                });
            }

            quote! {
                std::sync::Arc::new(Self {
                    #(#field_inits),*
                })
            }
        }
        syn::Fields::Unnamed(_) | syn::Fields::Unit => {
            // Tuple struct or unit struct - use Default
            quote! {
                std::sync::Arc::new(Self::default())
            }
        }
    };

    let expanded = quote! {
        #struct_def

        #extra_traits

        #default_impl

        // Type-based DI: impl Provided for the struct
        impl service_daemon::Provided for #struct_name {
            fn resolve() -> std::sync::Arc<Self> {
                #constructor
            }
        }
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
            // Build a map of provider name -> type_name for type checking
            let mut provider_types: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for entry in service_daemon::PROVIDER_REGISTRY.iter() {
                provider_types.insert(entry.name.to_string(), entry.type_name.to_string());
            }

            // Check service dependencies (name and type)
            for service in service_daemon::SERVICE_REGISTRY.iter() {
                for param in service.params {
                    let param_name = param.name.to_string();
                    let expected_type = param.type_name.to_string();

                    match provider_types.get(&param_name) {
                        None => {
                            tracing::warn!(
                                "⚠️  Service '{}' requires '{}' but no #[provider] found for it.",
                                service.name, param_name
                            );
                        }
                        Some(provided_type) => {
                            // Normalize types for comparison (remove common wrappers and namespaces)
                            let normalized_expected = expected_type
                                .trim_start_matches("Arc<")
                                .trim_end_matches(">")
                                .replace("tokio::sync::", "")
                                .replace("std::sync::", "")
                                .replace(" ", "");
                            let normalized_provided = provided_type
                                .replace("tokio::sync::", "")
                                .replace("std::sync::", "")
                                .replace(" ", "");

                            if normalized_expected != normalized_provided {
                                tracing::warn!(
                                    "⚠️  Type mismatch for '{}' in service '{}': expected '{}', provider gives '{}'",
                                    param_name, service.name, normalized_expected, normalized_provided
                                );
                            }
                        }
                    }
                }
            }

            // Check trigger targets (name and type)
            for trigger in service_daemon::TRIGGER_REGISTRY.iter() {
                let target_name = trigger.target.to_string();
                let expected_type = trigger.target_type.to_string();

                match provider_types.get(&target_name) {
                    None => {
                        tracing::warn!(
                            "⚠️  Trigger '{}' requires target '{}' but no #[provider] found for it.",
                            trigger.name, target_name
                        );
                    }
                    Some(provided_type) => {
                        // Normalize types for comparison
                        let normalized_expected = expected_type
                            .replace("tokio::sync::", "")
                            .replace("std::sync::", "")
                            .replace(" ", "");
                        let normalized_provided = provided_type
                            .replace("tokio::sync::", "")
                            .replace("std::sync::", "")
                            .replace(" ", "");

                        // Precise check first
                        if normalized_expected != normalized_provided {
                            // Fallback to contains check for complex generic types if they don't exactly match after normalization
                            if !normalized_provided.contains(&normalized_expected) && !normalized_expected.contains(&normalized_provided) {
                                tracing::warn!(
                                    "⚠️  Type mismatch for trigger '{}' target '{}': expected '{}', provider gives '{}'",
                                    trigger.name, target_name, normalized_expected, normalized_provided
                                );
                            }
                        }
                    }
                }

                // Check trigger DI parameters (index 2+)
                for param in trigger.params {
                    let param_name = param.name.to_string();
                    let expected_type = param.type_name.to_string();

                    match provider_types.get(&param_name) {
                        None => {
                            tracing::warn!(
                                "⚠️  Trigger '{}' requires dependency '{}' but no #[provider] found for it.",
                                trigger.name, param_name
                            );
                        }
                        Some(provided_type) => {
                            // Normalize types for comparison (remove Arc wrapper from expected)
                            let normalized_expected = expected_type
                                .trim_start_matches("Arc<")
                                .trim_end_matches(">")
                                .replace("tokio::sync::", "")
                                .replace("std::sync::", "")
                                .replace(" ", "");
                            let normalized_provided = provided_type
                                .replace("tokio::sync::", "")
                                .replace("std::sync::", "")
                                .replace(" ", "");

                            if normalized_expected != normalized_provided {
                                tracing::warn!(
                                    "⚠️  Type mismatch for '{}' in trigger '{}': expected '{}', provider gives '{}'",
                                    param_name, trigger.name, normalized_expected, normalized_provided
                                );
                            }
                        }
                    }
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

/// Marks a function as an event-driven trigger.
///
/// The macro automatically registers the trigger in the global registry
/// using `linkme`. The trigger will be started by `ServiceDaemon` and will
/// execute the function when the specified event occurs.
///
/// # Attributes
/// - `template`: The trigger template type. Options: "custom", "queue", "cron"
/// - `target`: The name of a provider that supplies the event source
///
/// # Template Types
/// - `custom`: Uses `tokio::sync::Notify`. Target should provide `Arc<Notify>`.
/// - `queue`: Uses `tokio::sync::mpsc::Receiver<T>`. Target should provide `Arc<Mutex<Receiver<T>>>`.
/// - `cron`: Uses cron expressions. Target should provide `&'static str` (the cron expression).
///
/// # Example
/// ```rust
/// use service_daemon::trigger;
///
/// #[trigger(template = "custom", target = "my_notifier")]
/// async fn on_custom_event(request: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Custom event triggered! ID: {}", trigger_id);
///     Ok(())
/// }
///
/// #[trigger(template = "queue", target = "task_queue")]
/// async fn on_queue_item(item: String, trigger_id: String) -> anyhow::Result<()> {
///     println!("Received queue item: {} (trigger: {})", item, trigger_id);
///     Ok(())
/// }
///
/// #[trigger(template = "cron", target = "cleanup_schedule")]
/// async fn on_cron_tick(tick_time: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Cron triggered at scheduled time! ID: {}", trigger_id);
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn trigger(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;

    // Parse attributes
    let attr_str = attr.to_string();
    let template =
        parse_trigger_attr(&attr_str, "template").unwrap_or_else(|| "custom".to_string());

    // Parse target as a type identifier (not a string)
    let target_str =
        parse_trigger_attr(&attr_str, "target").unwrap_or_else(|| "Unknown".to_string());
    let target_type: proc_macro2::TokenStream =
        target_str.parse().unwrap_or_else(|_| quote!(Unknown));

    // Extract the first parameter type (payload type)
    let payload_type: Option<Box<Type>> = sig.inputs.iter().find_map(|arg| {
        if let FnArg::Typed(pat_type) = arg {
            Some(pat_type.ty.clone())
        } else {
            None
        }
    });

    let payload_type_token = payload_type
        .clone()
        .unwrap_or_else(|| Box::new(syn::parse_quote!(())));
    let payload_type_str = quote!(#payload_type_token).to_string().replace(" ", "");

    // Compute expected target type based on template
    let target_type_str = match template.as_str() {
        "custom" => "tokio::sync::Notify".to_string(),
        "queue" => format!(
            "tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{}>>",
            payload_type_str
        ),
        "cron" => "String".to_string(),
        _ => "unknown".to_string(),
    };

    // Extract additional DI parameters (index 2+)
    let mut di_resolve_tokens = Vec::new();
    let mut di_call_args = Vec::new();
    let mut param_entries = Vec::new();

    for (i, arg) in sig.inputs.iter().enumerate() {
        // Skip first two params (payload and trigger_id)
        if i < 2 {
            continue;
        }

        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let arg_name = &pat_ident.ident;
                let arg_type = &pat_type.ty;
                let arg_name_str = arg_name.to_string();
                let arg_type_str = quote!(#arg_type).to_string().replace(" ", "");

                // Add to param entries for verification
                param_entries.push(quote! {
                    service_daemon::ServiceParam {
                        name: #arg_name_str,
                        type_name: #arg_type_str,
                    }
                });

                // Check if the type is Arc<T>
                if let Type::Path(type_path) = &**arg_type {
                    if let Some(segment) = type_path.path.segments.last() {
                        if segment.ident == "Arc" {
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                if let Some(syn::GenericArgument::Type(inner_type)) =
                                    args.args.first()
                                {
                                    // Type-Based DI: use T::resolve() for compile-time verification
                                    di_resolve_tokens.push(quote! {
                                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve();
                                    });
                                    di_call_args.push(quote! { #arg_name });
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Non-Arc types are not supported for DI
                let error = syn::Error::new_spanned(
                    arg_type,
                    "Trigger DI parameters must be Arc<T> where T implements Provided",
                );
                return TokenStream::from(error.to_compile_error());
            }
        }
    }

    let wrapper_name = format_ident!("{}_trigger_wrapper", fn_name);
    let entry_name = format_ident!("__TRIGGER_ENTRY_{}", fn_name.to_string().to_uppercase());

    // Generate template-specific event loop
    let event_loop = match template.as_str() {
        "custom" => {
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Custom trigger - resolve target using Type-Based DI
                let notifier_wrapper = <#target_type as service_daemon::Provided>::resolve();

                loop {
                    notifier_wrapper.notified().await;
                    let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                    tracing::info!("Custom trigger '{}' fired (ID: {})", #fn_name_str, trigger_id);
                    if let Err(e) = #fn_name((), trigger_id, #(#di_call_args.clone()),*).await {
                        tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                    }
                }
            }
        }
        "queue" => {
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Queue trigger - resolve target using Type-Based DI
                let queue_wrapper = <#target_type as service_daemon::Provided>::resolve();

                loop {
                    let item = {
                        let mut receiver = queue_wrapper.lock().await;
                        receiver.recv().await
                    };

                    match item {
                        Some(value) => {
                            let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                            tracing::info!("Queue trigger '{}' received item (ID: {})", #fn_name_str, trigger_id);
                            if let Err(e) = #fn_name(value, trigger_id, #(#di_call_args.clone()),*).await {
                                tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                            }
                        }
                        None => {
                            tracing::warn!("Queue trigger '{}' channel closed", #fn_name_str);
                            break;
                        }
                    }
                }
            }
        }
        "cron" => {
            // For cron, we need to clone the Arc values into the closure
            let di_clone_for_cron: Vec<_> = di_call_args
                .iter()
                .map(|arg| {
                    quote! { let #arg = #arg.clone(); }
                })
                .collect();

            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Cron trigger - resolve target using Type-Based DI
                use service_daemon::tokio_cron_scheduler::{Job, JobScheduler};

                let schedule_wrapper = <#target_type as service_daemon::Provided>::resolve();

                let sched = JobScheduler::new().await?;

                let fn_name_for_job = #fn_name_str;
                #(#di_clone_for_cron)*
                let job = Job::new_async(schedule_wrapper.as_str(), move |_uuid, _lock| {
                    let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                    let fn_name_clone = fn_name_for_job;
                    #(let #di_call_args = #di_call_args.clone();)*
                    Box::pin(async move {
                        tracing::info!("Cron trigger '{}' fired (ID: {})", fn_name_clone, trigger_id);
                        if let Err(e) = #fn_name((), trigger_id, #(#di_call_args),*).await {
                            tracing::error!("Trigger '{}' error: {:?}", fn_name_clone, e);
                        }
                    })
                })?;

                sched.add(job).await?;
                sched.start().await?;

                // Keep the scheduler running
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                }
            }
        }
        _ => {
            quote! {
                anyhow::bail!("Unknown trigger template: {}", #template);
            }
        }
    };

    let expanded = quote! {
        #(#attrs)*
        #vis #sig {
            #body
        }

        /// Auto-generated trigger wrapper - runs the event loop
        pub fn #wrapper_name() -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #event_loop
                #[allow(unreachable_code)]
                Ok(())
            })
        }

        /// Auto-generated static trigger entry - collected by linkme at link time
        #[service_daemon::linkme::distributed_slice(service_daemon::TRIGGER_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::TriggerEntry = service_daemon::TriggerEntry {
            name: #fn_name_str,
            template: #template,
            target: #target_str,
            target_type: #target_type_str,
            payload_type: #payload_type_str,
            params: &[#(#param_entries),*],
            run: #wrapper_name,
        };
    };

    TokenStream::from(expanded)
}

fn parse_trigger_attr(attr_str: &str, key: &str) -> Option<String> {
    // Parse: key = "value"
    for part in attr_str.split(',') {
        let part = part.trim();
        if part.contains(key) {
            if let Some(value_part) = part.split('=').nth(1) {
                return Some(value_part.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

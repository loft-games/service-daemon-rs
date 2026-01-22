use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, ItemStruct, Pat, Type, parse_macro_input};

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

/// Marks a struct as a type-based dependency provider.
///
/// The macro automatically implements `Provided` for the struct, enabling
/// compile-time verified dependency injection.
///
/// # Example with default value
/// ```rust
/// use service_daemon::provider;
///
/// #[provider(default = 8080)]
/// pub struct Port(pub i32);
///
/// #[provider(default = "mysql://localhost")]  // Auto-expands to .to_owned()
/// pub struct DbUrl(pub String);
/// ```
///
/// # Example with environment variable
/// ```rust
/// use service_daemon::provider;
///
/// #[provider(default = "localhost:5432", env_name = "DATABASE_HOST")]
/// pub struct DatabaseHost(pub String);
/// ```
///
/// # Example with dependencies
/// ```rust
/// use service_daemon::provider;
/// use std::sync::Arc;
///
/// #[provider]
/// pub struct AppConfig {
///     pub port: Arc<Port>,
///     pub db_url: Arc<DbUrl>,
/// }
/// ```
#[proc_macro_attribute]
pub fn provider(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_str = attr.to_string();

    // Support struct items for type-based DI
    if let Ok(item_struct) = syn::parse::<ItemStruct>(item.clone()) {
        return generate_struct_provider(item_struct, &attr_str);
    }

    // Support async fn items for custom async initialization
    if let Ok(item_fn) = syn::parse::<ItemFn>(item.clone()) {
        return generate_async_fn_provider(item_fn);
    }

    // Error for unsupported items
    let error = syn::Error::new_spanned(
        proc_macro2::TokenStream::from(item),
        "#[provider] can only be applied to struct or async fn items.",
    );
    TokenStream::from(error.to_compile_error())
}

/// Parsed attributes from #[provider(...)]
struct ProviderAttrs {
    default_value: Option<String>,
    env_name: Option<String>,
    /// Only applies to Queue template (e.g., item_type = "MyTask")
    item_type: Option<String>,
    /// Capacity for Queue template (default 100)
    capacity: Option<usize>,
}

/// Parses attributes from #[provider(default = ..., env_name = "...", item_type = "...", capacity = N)]
fn parse_provider_attrs(attr_str: &str) -> ProviderAttrs {
    let mut attrs = ProviderAttrs {
        default_value: None,
        env_name: None,
        item_type: None,
        capacity: None,
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
                    "item_type" => attrs.item_type = Some(val.trim_matches('"').to_string()),
                    "capacity" => attrs.capacity = val.parse().ok(),
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
            "item_type" => attrs.item_type = Some(val.trim_matches('"').to_string()),
            "capacity" => attrs.capacity = val.parse().ok(),
            _ => {}
        }
    }

    attrs
}

/// Generates a provider from an async function.
///
/// This allows custom async initialization logic:
/// ```rust
/// #[provider]
/// async fn my_config() -> AppConfig {
///     let db = connect_db().await;
///     AppConfig { db, port: 8080 }
/// }
/// ```
///
/// The function is called once using `tokio::sync::OnceCell` for async singleton behavior.
fn generate_async_fn_provider(item_fn: ItemFn) -> TokenStream {
    let fn_name = &item_fn.sig.ident;
    let fn_vis = &item_fn.vis;
    let fn_block = &item_fn.block;
    let fn_asyncness = &item_fn.sig.asyncness;

    // Extract return type
    let return_type = match &item_fn.sig.output {
        syn::ReturnType::Type(_, ty) => ty.clone(),
        syn::ReturnType::Default => {
            let error = syn::Error::new_spanned(
                &item_fn.sig,
                "#[provider] async fn must have a return type",
            );
            return TokenStream::from(error.to_compile_error());
        }
    };

    // Verify it's async
    if fn_asyncness.is_none() {
        let error =
            syn::Error::new_spanned(&item_fn.sig, "#[provider] on functions requires async fn");
        return TokenStream::from(error.to_compile_error());
    }

    let singleton_name = format_ident!("__ASYNC_SINGLETON_{}", fn_name.to_string().to_uppercase());

    let expanded = quote! {
        // Keep the original function (private impl detail)
        #fn_vis async fn #fn_name() -> #return_type
            #fn_block

        // Generate Provided impl for the return type using OnceCell
        impl service_daemon::Provided for #return_type {
            fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: tokio::sync::OnceCell<std::sync::Arc<#return_type>> = tokio::sync::OnceCell::const_new();

                // Use blocking to get the value synchronously (required for Provided trait)
                // The actual async init happens on first call.
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        #singleton_name.get_or_init(|| async {
                            std::sync::Arc::new(#fn_name().await)
                        }).await.clone()
                    })
                })
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Signal provider using `tokio::sync::Notify`.

///
/// This is triggered by `#[provider(default = Notify)]`.
/// The struct will have:
/// - An inner `Arc<Notify>`
/// - A static `notify()` method to call `notify_one()` from anywhere
/// - `Deref` to `Notify` for direct access
fn generate_notify_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
) -> TokenStream {
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());

    let expanded = quote! {
        #(#attrs)*
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

        impl service_daemon::Provided for #struct_name {
            fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: std::sync::OnceLock<std::sync::Arc<#struct_name>> = std::sync::OnceLock::new();
                #singleton_name.get_or_init(|| std::sync::Arc::new(Self::default())).clone()
            }
        }

        impl #struct_name {
            /// Trigger this signal from anywhere in the application.
            /// This is a static shortcut that resolves the singleton and calls `notify_one()`.
            pub fn notify() {
                <Self as service_daemon::Provided>::resolve().notify_one();
            }

            /// Wait for a notification on this signal.
            /// This is a static shortcut that resolves the singleton and calls `notified().await`.
            pub async fn wait() {
                <Self as service_daemon::Provided>::resolve().notified().await;
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Load-Balancing Queue provider using `tokio::sync::mpsc`.
///
/// This is triggered by `#[provider(default = LoadBalancingQueue)]` or `#[provider(default = LBQueue)]`.
/// - Uses `mpsc::channel` (single consumer).
/// - Messages are distributed to one handler at a time.
/// - Ideal for task distribution across workers.
fn generate_lb_queue_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    item_type_str: &str,
    capacity: usize,
) -> TokenStream {
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());
    let item_type: proc_macro2::TokenStream =
        item_type_str.parse().unwrap_or_else(|_| quote!(String));

    let expanded = quote! {
        #(#attrs)*
        #vis struct #struct_name {
            pub tx: tokio::sync::mpsc::Sender<#item_type>,
            pub rx: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<#item_type>>>,
        }

        impl Default for #struct_name {
            fn default() -> Self {
                let (tx, rx) = tokio::sync::mpsc::channel(#capacity);
                Self {
                    tx,
                    rx: std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
                }
            }
        }

        impl std::ops::Deref for #struct_name {
            type Target = std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<#item_type>>>;
            fn deref(&self) -> &Self::Target {
                &self.rx
            }
        }

        impl service_daemon::Provided for #struct_name {
            fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: std::sync::OnceLock<std::sync::Arc<#struct_name>> = std::sync::OnceLock::new();
                #singleton_name.get_or_init(|| std::sync::Arc::new(Self::default())).clone()
            }
        }

        impl #struct_name {
            /// Push an item to this queue from anywhere in the application.
            /// This is a static shortcut that resolves the singleton and calls `tx.send(item)`.
            pub async fn push(item: #item_type) -> Result<(), tokio::sync::mpsc::error::SendError<#item_type>> {
                <Self as service_daemon::Provided>::resolve().tx.send(item).await
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Broadcast Queue provider using `tokio::sync::broadcast`.
///
/// This is triggered by `#[provider(default = BroadcastQueue)]`, `#[provider(default = Queue)]`, or `#[provider(default = BQueue)]`.
/// - Uses `broadcast::channel` (multiple subscribers).
/// - All triggers receive every message (fanout).
/// - Ideal for event notification to multiple handlers.
/// - Item type must implement `Clone`.
fn generate_broadcast_queue_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    item_type_str: &str,
    capacity: usize,
) -> TokenStream {
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());
    let item_type: proc_macro2::TokenStream =
        item_type_str.parse().unwrap_or_else(|_| quote!(String));

    let expanded = quote! {
        #(#attrs)*
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
            fn deref(&self) -> &Self::Target {
                &self.tx
            }
        }

        impl service_daemon::Provided for #struct_name {
            fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: std::sync::OnceLock<std::sync::Arc<#struct_name>> = std::sync::OnceLock::new();
                #singleton_name.get_or_init(|| std::sync::Arc::new(Self::default())).clone()
            }
        }

        impl #struct_name {
            /// Push an item to this queue from anywhere in the application.
            /// All subscribed handlers will receive a copy of this item.
            pub fn push(item: #item_type) -> Result<usize, tokio::sync::broadcast::error::SendError<#item_type>> {
                <Self as service_daemon::Provided>::resolve().tx.send(item)
            }

            /// Subscribe to this queue to receive broadcast messages.
            /// Each trigger should call this to get its own receiver.
            pub fn subscribe() -> tokio::sync::broadcast::Receiver<#item_type> {
                <Self as service_daemon::Provided>::resolve().tx.subscribe()
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a provider for a struct with automatic field injection.

///
/// For each field of type `Arc<T>`, it calls `T::resolve()` to inject dependencies.
/// For single-element tuple structs with `default = ...`, generates Deref, Display, Default.
/// Uses `once_cell` for singleton behavior so the same instance is returned on each resolve.
///
/// # Magic Templates
/// - `#[provider(default = Notify)]` - Generates a Signal with `notify()` static method.
/// - `#[provider(default = Queue, item_type = "T")]` - Generates a Channel with `push(item)` static method.
fn generate_struct_provider(item: ItemStruct, attr_str: &str) -> TokenStream {
    let struct_name = &item.ident;
    let vis = &item.vis;
    let attrs = &item.attrs;
    let generics = &item.generics;
    let fields = &item.fields;
    let semi = &item.semi_token;

    // Parse attributes (default, env_name, item_type, capacity)
    let provider_attrs = parse_provider_attrs(attr_str);

    // Check for magic template defaults
    if let Some(ref default_val) = provider_attrs.default_value {
        match default_val.as_str() {
            // Signal templates
            "Notify" | "Event" => {
                return generate_notify_template(struct_name, vis, attrs);
            }
            // Broadcast queue templates (fanout - all handlers receive the event)
            "BroadcastQueue" | "Queue" | "BQueue" => {
                let item_type_str = provider_attrs.item_type.as_deref().unwrap_or("String");
                let capacity = provider_attrs.capacity.unwrap_or(100);
                return generate_broadcast_queue_template(
                    struct_name,
                    vis,
                    attrs,
                    item_type_str,
                    capacity,
                );
            }
            // Load-balancing queue templates (single consumer)
            "LoadBalancingQueue" | "LBQueue" => {
                let item_type_str = provider_attrs.item_type.as_deref().unwrap_or("String");
                let capacity = provider_attrs.capacity.unwrap_or(100);
                return generate_lb_queue_template(
                    struct_name,
                    vis,
                    attrs,
                    item_type_str,
                    capacity,
                );
            }
            _ => {}
        }
    }

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

    // Check if inner type is String (for auto .to_owned() expansion)
    let inner_is_string = inner_type.as_ref().map_or(false, |ty| {
        if let syn::Type::Path(type_path) = ty {
            type_path
                .path
                .segments
                .last()
                .map_or(false, |seg| seg.ident == "String")
        } else {
            false
        }
    });

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
                // Auto-expand string literals to .to_owned() if inner type is String
                let default_tokens: proc_macro2::TokenStream = if inner_is_string
                    && default_val.as_str().starts_with('"')
                    && default_val.as_str().ends_with('"')
                    && !default_val.contains(".to_")
                {
                    // It's a bare string literal for a String field - add .to_owned()
                    format!("{}.to_owned()", default_val)
                        .parse()
                        .unwrap_or_else(|_| quote! { Default::default() })
                } else {
                    default_val
                        .parse()
                        .unwrap_or_else(|_| quote! { Default::default() })
                };
                quote! {
                    std::env::var(#env_name).unwrap_or_else(|_| #default_tokens)
                }
            } else {
                quote! {
                    std::env::var(#env_name).expect(concat!("Environment variable ", #env_name, " not set"))
                }
            }
        } else if let Some(ref default_val) = provider_attrs.default_value {
            // Auto-expand string literals to .to_owned() if inner type is String
            let default_tokens: proc_macro2::TokenStream = if inner_is_string
                && default_val.as_str().starts_with('"')
                && default_val.as_str().ends_with('"')
                && !default_val.contains(".to_")
            {
                // It's a bare string literal for a String field - add .to_owned()
                format!("{}.to_owned()", default_val)
                    .parse()
                    .unwrap_or_else(|_| quote! { Default::default() })
            } else {
                default_val
                    .parse()
                    .unwrap_or_else(|_| quote! { Default::default() })
            };
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

    // Generate unique static name for singleton
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());

    let expanded = quote! {
        #struct_def

        #extra_traits

        #default_impl

        // Type-based DI: impl Provided for the struct with singleton behavior
        impl service_daemon::Provided for #struct_name {
            fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: std::sync::OnceLock<std::sync::Arc<#struct_name>> = std::sync::OnceLock::new();
                #singleton_name.get_or_init(|| {
                    #constructor
                }).clone()
            }
        }
    };

    TokenStream::from(expanded)
}

/// **DEPRECATED**: This macro is a no-op.
///
/// With Type-Based DI, compile-time checks are automatic via the `Provided` trait.
/// If a service requires `Arc<T>` but no `#[provider]` for `T` exists,
/// you get a compile error: "Missing Provider: The type `T` cannot be injected."
///
/// No runtime verification is needed.
#[proc_macro]
pub fn verify_setup(_input: TokenStream) -> TokenStream {
    // No-op - compile-time checks via Provided trait are sufficient
    TokenStream::new()
}

/// Marks a function as an event-driven trigger.
///
/// The macro automatically registers the trigger in the global registry
/// using `linkme`. The trigger will be started by `ServiceDaemon` and will
/// execute the function when the specified event occurs.
///
/// # Attributes
/// - `template`: The trigger template type. Options: `cron`, `queue`, `lb_queue`, `event`, `notify`, `custom`.
/// - `target`: The provider type (struct) that supplies the event source.
///
/// # Template Types
/// - `cron`: Uses cron expressions. Target should be a provider for `String` (the cron expression).
/// - `queue`: Broadcast queue (fanout). Target should be a `#[provider(default = Queue)]`.
/// - `lb_queue`: Load-balancing queue. Target should be a `#[provider(default = LBQueue)]`.
/// - `event` / `notify` / `custom`: Signal trigger. Target should be a `#[provider(default = Notify)]`.
///
/// # Example
/// ```rust
/// use service_daemon::trigger;
///
/// #[trigger(template = event, target = MyNotifier)]
/// async fn on_event(payload: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Event triggered! ID: {}", trigger_id);
///     Ok(())
/// }
///
/// #[trigger(template = queue, target = TaskQueue)]
/// async fn on_queue_item(item: String, trigger_id: String) -> anyhow::Result<()> {
///     println!("Received queue item: {} (trigger: {})", item, trigger_id);
///     Ok(())
/// }
///
/// #[trigger(template = cron, target = CleanupSchedule)]
/// async fn on_cron_tick(tick_time: (), trigger_id: String) -> anyhow::Result<()> {
///     println!("Cron triggered! ID: {}", trigger_id);
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
    let _target_type_str = match template.as_str() {
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
        // Signal/Notify triggers - aliases: custom, notify, event
        "custom" | "notify" | "event" => {
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // Signal trigger - resolve target using Type-Based DI
                let notifier_wrapper = <#target_type as service_daemon::Provided>::resolve();

                loop {
                    notifier_wrapper.notified().await;
                    let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                    tracing::info!("Signal trigger '{}' fired (ID: {})", #fn_name_str, trigger_id);
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

                // Broadcast Queue trigger - each handler subscribes independently
                // Subscribe to get our own receiver
                let mut receiver = #target_type::subscribe();

                loop {
                    match receiver.recv().await {
                        Ok(value) => {
                            let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                            tracing::info!("Queue trigger '{}' received item (ID: {})", #fn_name_str, trigger_id);
                            if let Err(e) = #fn_name(value, trigger_id, #(#di_call_args.clone()),*).await {
                                tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Queue trigger '{}' lagged by {} messages", #fn_name_str, n);
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::warn!("Queue trigger '{}' channel closed", #fn_name_str);
                            break;
                        }
                    }
                }
            }
        }
        // Load-balancing queue trigger - all triggers share one receiver
        "lb_queue" => {
            quote! {
                // Resolve DI dependencies for additional parameters
                #(#di_resolve_tokens)*

                // LB Queue trigger - use shared receiver via lock
                let queue_wrapper = <#target_type as service_daemon::Provided>::resolve();

                loop {
                    let item = {
                        let mut receiver = queue_wrapper.rx.lock().await;
                        receiver.recv().await
                    };

                    match item {
                        Some(value) => {
                            let trigger_id = service_daemon::uuid::Uuid::new_v4().to_string();
                            tracing::info!("LB Queue trigger '{}' received item (ID: {})", #fn_name_str, trigger_id);
                            if let Err(e) = #fn_name(value, trigger_id, #(#di_call_args.clone()),*).await {
                                tracing::error!("Trigger '{}' error: {:?}", #fn_name_str, e);
                            }
                        }
                        None => {
                            tracing::warn!("LB Queue trigger '{}' channel closed", #fn_name_str);
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

        /// Auto-generated trigger wrapper - acts as an event-loop "Call Host"
        /// This is registered as a Service, so it benefits from ServiceDaemon's lifecycle management.
        pub fn #wrapper_name() -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #event_loop
                #[allow(unreachable_code)]
                Ok(())
            })
        }

        /// Auto-generated static registry entry - triggers are specialized services
        #[service_daemon::linkme::distributed_slice(service_daemon::SERVICE_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ServiceEntry = service_daemon::ServiceEntry {
            name: #fn_name_str,
            module: concat!("triggers/", #template),
            params: &[#(#param_entries),*],
            wrapper: #wrapper_name,
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

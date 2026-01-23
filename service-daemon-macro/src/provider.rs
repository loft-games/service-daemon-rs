//! `#[provider]` macro implementation.

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::{ItemFn, ItemStruct, Type};

use crate::common::has_allow_sync;

/// Implementation of the `#[provider]` attribute macro.
pub fn provider_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_str = attr.to_string();

    // Support struct items for type-based DI
    if let Ok(item_struct) = syn::parse::<ItemStruct>(item.clone()) {
        return generate_struct_provider(item_struct, &attr_str);
    }

    // Support async fn items for custom async initialization
    if let Ok(item_fn) = syn::parse::<ItemFn>(item.clone()) {
        return generate_async_fn_provider(item_fn);
    }

    // Error for unsupported items - use abort! for enhanced error
    abort!(
        proc_macro2::TokenStream::from(item),
        "#[provider] can only be applied to struct or async fn items";
        help = "Use #[provider] on a struct definition or an async function";
        note = "Example: #[provider(default = 8080)] pub struct Port(pub i32);"
    )
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

    // Helper to apply a key-value pair to attrs
    let mut apply_kv = |key: &str, val: String| match key.trim().to_lowercase().as_str() {
        "default" | "value" => attrs.default_value = Some(val),
        "env_name" => attrs.env_name = Some(val.trim_matches('"').to_string()),
        "item_type" => attrs.item_type = Some(val.trim_matches('"').to_string()),
        "capacity" => attrs.capacity = val.parse().ok(),
        _ => {}
    };

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
                apply_kv(&current_key, current_value.trim().to_string());
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
        apply_kv(&current_key, current_value.trim().to_string());
    }

    attrs
}

/// Generates a provider from an async function.
fn generate_async_fn_provider(item_fn: ItemFn) -> TokenStream {
    let fn_name = &item_fn.sig.ident;
    let fn_vis = &item_fn.vis;
    let fn_block = &item_fn.block;
    let fn_asyncness = &item_fn.sig.asyncness;

    // Extract return type
    let return_type = match &item_fn.sig.output {
        syn::ReturnType::Type(_, ty) => ty.clone(),
        syn::ReturnType::Default => {
            abort!(
                &item_fn.sig,
                "#[provider] async fn must have a return type";
                help = "Add a return type, e.g., `async fn config() -> MyConfig { ... }`"
            );
        }
    };

    let fn_name_str = fn_name.to_string();
    let return_type_str = quote!(#return_type).to_string();
    let allow_sync_present = has_allow_sync(&item_fn.attrs);
    let call_expr = if fn_asyncness.is_some() {
        quote! { #fn_name().await }
    } else if allow_sync_present {
        // User explicitly allowed sync, no warning
        quote! { #fn_name() }
    } else {
        quote! {
            {
                tracing::warn!("Provider function '{}' for type '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str, #return_type_str);
                #fn_name()
            }
        }
    };

    let singleton_name = format_ident!("__ASYNC_SINGLETON_{}", fn_name.to_string().to_uppercase());

    let expanded = quote! {
        // Keep the original function (private impl detail)
        #fn_vis #fn_asyncness fn #fn_name() -> #return_type
            #fn_block

        // Generate Provided impl for the return type using OnceCell (fully async)
        impl service_daemon::Provided for #return_type {
            async fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: tokio::sync::OnceCell<std::sync::Arc<#return_type>> = tokio::sync::OnceCell::const_new();

                #singleton_name.get_or_init(|| async {
                    std::sync::Arc::new(#call_expr)
                }).await.clone()
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Signal provider using `tokio::sync::Notify`.
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
            async fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: tokio::sync::OnceCell<std::sync::Arc<#struct_name>> = tokio::sync::OnceCell::const_new();
                #singleton_name.get_or_init(|| async { std::sync::Arc::new(Self::default()) }).await.clone()
            }
        }

        impl #struct_name {
            /// Trigger this signal from anywhere in the application.
            pub async fn notify() {
                <Self as service_daemon::Provided>::resolve().await.notify_one();
            }

            /// Wait for a notification on this signal.
            pub async fn wait() {
                <Self as service_daemon::Provided>::resolve().await.notified().await;
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Load-Balancing Queue provider using `tokio::sync::mpsc`.
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
            async fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: tokio::sync::OnceCell<std::sync::Arc<#struct_name>> = tokio::sync::OnceCell::const_new();
                #singleton_name.get_or_init(|| async { std::sync::Arc::new(Self::default()) }).await.clone()
            }
        }

        impl #struct_name {
            /// Push an item to this queue from anywhere in the application.
            pub async fn push(item: #item_type) -> Result<(), tokio::sync::mpsc::error::SendError<#item_type>> {
                <Self as service_daemon::Provided>::resolve().await.tx.send(item).await
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a Broadcast Queue provider using `tokio::sync::broadcast`.
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
            async fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: tokio::sync::OnceCell<std::sync::Arc<#struct_name>> = tokio::sync::OnceCell::const_new();
                #singleton_name.get_or_init(|| async { std::sync::Arc::new(Self::default()) }).await.clone()
            }
        }

        impl #struct_name {
            /// Push an item to this queue from anywhere in the application.
            pub async fn push(item: #item_type) -> Result<usize, tokio::sync::broadcast::error::SendError<#item_type>> {
                <Self as service_daemon::Provided>::resolve().await.tx.send(item)
            }

            /// Subscribe to this queue to receive broadcast messages.
            pub async fn subscribe() -> tokio::sync::broadcast::Receiver<#item_type> {
                <Self as service_daemon::Provided>::resolve().await.tx.subscribe()
            }
        }
    };

    TokenStream::from(expanded)
}

/// Generates a provider for a struct with automatic field injection.
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
    let inner_is_string = inner_type.as_ref().is_some_and(|ty| {
        if let syn::Type::Path(type_path) = ty
            && let Some(seg) = type_path.path.segments.last()
        {
            seg.ident == "String"
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

    // Helper to generate the to_owned() expansion if needed
    let dot_owned_expansion = |val: &String| {
        if inner_is_string
            && val.as_str().starts_with('"')
            && val.as_str().ends_with('"')
            && !val.contains(".to_")
        {
            // It's a bare string literal for a String field - add .to_owned()
            format!("{}.to_owned()", val)
                .parse()
                .unwrap_or_else(|_| quote! { Default::default() })
        } else {
            val.parse()
                .unwrap_or_else(|_| quote! { Default::default() })
        }
    };

    // Generate Default impl for single-element tuple structs
    let default_impl = if let Some(ref _inner) = inner_type {
        // Build the default expression
        let default_expr = if let Some(ref env_name) = provider_attrs.env_name {
            // Use env var with fallback to default
            if let Some(ref default_val) = provider_attrs.default_value {
                let default_tokens = dot_owned_expansion(default_val);
                quote! {
                    std::env::var(#env_name).unwrap_or_else(|_| #default_tokens)
                }
            } else {
                quote! {
                    std::env::var(#env_name).expect(concat!("Environment variable ", #env_name, " not set"))
                }
            }
        } else if let Some(ref default_val) = provider_attrs.default_value {
            let default_tokens = dot_owned_expansion(default_val);
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

    // Generate constructor based on field type (async for named structs with Arc deps)
    let constructor = match fields {
        syn::Fields::Named(named_fields) => {
            let mut field_inits = Vec::new();
            for field in &named_fields.named {
                let field_name = field.ident.as_ref().unwrap();
                let field_type = &field.ty;

                // Check if it's Arc<T>
                if let Type::Path(syn::TypePath { path, .. }) = field_type
                    && let (Some(segment), true) = (path.segments.last(), path.segments.len() == 1)
                    && segment.ident == "Arc"
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
                {
                    // Async resolution with .await
                    field_inits.push(quote! {
                        #field_name: <#inner_type as service_daemon::Provided>::resolve().await
                    });
                    continue;
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
            // Tuple struct or unit struct - use Default (sync init is fine here)
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

        // Type-based DI: impl Provided for the struct with async singleton behavior
        impl service_daemon::Provided for #struct_name {
            async fn resolve() -> std::sync::Arc<Self> {
                static #singleton_name: tokio::sync::OnceCell<std::sync::Arc<#struct_name>> = tokio::sync::OnceCell::const_new();
                #singleton_name.get_or_init(|| async {
                    #constructor
                }).await.clone()
            }
        }
    };

    TokenStream::from(expanded)
}

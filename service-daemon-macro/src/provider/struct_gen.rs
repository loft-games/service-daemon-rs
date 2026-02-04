//! Struct provider generation.
//!
//! This module generates providers for structs with automatic field injection.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemStruct, Type};

use super::parser::{ProviderAttrs, parse_provider_attrs};
use super::templates::{
    generate_broadcast_queue_template, generate_lb_queue_template, generate_notify_template,
};

/// Generates a provider for a struct with automatic field injection.
pub fn generate_struct_provider(item: ItemStruct, attr_str: &str) -> TokenStream {
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
    let extra_traits = generate_extra_traits(&inner_type, struct_name);

    // Generate Default impl for single-element tuple structs
    let default_impl =
        generate_default_impl(&inner_type, inner_is_string, &provider_attrs, struct_name);

    // Generate constructor based on field type (async for named structs with Arc deps)
    let constructor = generate_constructor(fields);

    // Generate unique static name for singleton
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());

    let expanded = quote! {
        #struct_def

        #extra_traits

        #default_impl

        static #singleton_name: service_daemon::utils::managed_state::StateManager<#struct_name> = service_daemon::utils::managed_state::StateManager::new();

        // Type-based DI: impl Provided for the struct with intelligent state management
        impl service_daemon::Provided for #struct_name {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async {
                    #constructor
                }).await
            }

            async fn resolve_rwlock() -> std::sync::Arc<service_daemon::utils::managed_state::RwLock<Self>> {
                #singleton_name.resolve_rwlock(|| async {
                    #constructor
                }).await
            }

            async fn resolve_mutex() -> std::sync::Arc<service_daemon::utils::managed_state::Mutex<Self>> {
                #singleton_name.resolve_mutex(|| async {
                    #constructor
                }).await
            }

            async fn changed() {
                #singleton_name.changed().await
            }
        }

        impl #struct_name {
            /// Resolves a tracked RwLock for this provider.
            pub async fn rwlock() -> std::sync::Arc<service_daemon::utils::managed_state::RwLock<Self>> {
                <Self as service_daemon::Provided>::resolve_rwlock().await
            }

            /// Resolves a tracked Mutex for this provider.
            pub async fn mutex() -> std::sync::Arc<service_daemon::utils::managed_state::Mutex<Self>> {
                <Self as service_daemon::Provided>::resolve_mutex().await
            }
        }
    };

    TokenStream::from(quote! {
        #expanded
    })
}

/// Generates extra traits (Deref, Display) for single-element tuple structs.
fn generate_extra_traits(
    inner_type: &Option<syn::Type>,
    struct_name: &syn::Ident,
) -> proc_macro2::TokenStream {
    if let Some(inner) = inner_type {
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
    }
}

/// Generates the Default impl for single-element tuple structs.
fn generate_default_impl(
    inner_type: &Option<syn::Type>,
    inner_is_string: bool,
    provider_attrs: &ProviderAttrs,
    struct_name: &syn::Ident,
) -> proc_macro2::TokenStream {
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

    if inner_type.is_some() {
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
    }
}

/// Generates the constructor for the struct provider.
fn generate_constructor(fields: &syn::Fields) -> proc_macro2::TokenStream {
    match fields {
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
    }
}

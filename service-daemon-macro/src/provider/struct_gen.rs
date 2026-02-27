//! Struct provider generation.
//!
//! This module generates providers for structs with automatic field injection.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemStruct, Type};

use super::parser::{ProviderAttrs, parse_provider_attrs};
use super::templates::{generate_broadcast_queue_template, generate_notify_template};

/// Attempts to generate a template-based provider if the default value matches a known template.
/// Returns `Some(TokenStream)` if a template was matched, `None` otherwise.
fn try_generate_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    provider_attrs: &ProviderAttrs,
) -> Option<TokenStream> {
    let default_val = provider_attrs.default_value.as_ref()?;

    match default_val.as_str() {
        // Signal templates
        "Notify" | "Event" => Some(generate_notify_template(struct_name, vis, attrs)),
        // Broadcast queue templates (fanout - all handlers receive the event)
        "BroadcastQueue" | "Queue" | "BQueue" => {
            let item_type_str = provider_attrs.item_type.as_deref().unwrap_or("String");
            let capacity = provider_attrs.capacity.unwrap_or(100);
            Some(generate_broadcast_queue_template(
                struct_name,
                vis,
                attrs,
                item_type_str,
                capacity,
            ))
        }
        _ => None,
    }
}

/// Information about a single-element tuple struct.
struct TupleStructInfo {
    inner_type: syn::Type,
    is_string: bool,
}

impl TupleStructInfo {
    /// Analyzes fields to determine if this is a single-element tuple struct.
    fn from_fields(fields: &syn::Fields) -> Option<Self> {
        if let syn::Fields::Unnamed(f) = fields
            && f.unnamed.len() == 1
        {
            let first_field = f
                .unnamed
                .first()
                .expect("FieldsUnnamed checked for length 1");
            let inner_type = first_field.ty.clone();
            let is_string = Self::is_string_type(&inner_type);
            Some(Self {
                inner_type,
                is_string,
            })
        } else {
            None
        }
    }

    fn is_string_type(ty: &syn::Type) -> bool {
        if let syn::Type::Path(type_path) = ty
            && let Some(seg) = type_path.path.segments.last()
        {
            seg.ident == "String"
        } else {
            false
        }
    }
}

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

    // Check for magic template defaults first
    if let Some(template_output) = try_generate_template(struct_name, vis, attrs, &provider_attrs) {
        return template_output;
    }

    // Generate struct definition with proper syntax
    let struct_def = generate_struct_definition(attrs, vis, struct_name, generics, fields, semi);

    // Analyze tuple struct properties
    let tuple_info = TupleStructInfo::from_fields(fields);

    // Generate extra traits ONLY for single-element tuple structs
    let extra_traits = generate_extra_traits(&tuple_info, struct_name);

    // Generate Default impl for single-element tuple structs
    let default_impl = generate_default_impl(&tuple_info, &provider_attrs, struct_name);

    // Generate constructor based on field type (async for named structs with Arc deps)
    let constructor = generate_constructor(fields);

    // Generate unique static name for singleton
    let singleton_name = format_ident!("__SINGLETON_{}", struct_name.to_string().to_uppercase());

    let expanded = quote! {
        #struct_def

        #extra_traits

        #default_impl

        static #singleton_name: service_daemon::core::managed_state::StateManager<#struct_name> = service_daemon::core::managed_state::StateManager::new();

        // Type-based DI: impl Provided for the struct with intelligent state management
        impl service_daemon::Provided for #struct_name {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async {
                    #constructor
                }).await
            }

            async fn resolve_rwlock() -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                #singleton_name.resolve_rwlock(|| async {
                    #constructor
                }).await
            }

            async fn resolve_mutex() -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
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
            pub async fn rwlock() -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                <Self as service_daemon::Provided>::resolve_rwlock().await
            }

            /// Resolves a tracked Mutex for this provider.
            pub async fn mutex() -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
                <Self as service_daemon::Provided>::resolve_mutex().await
            }
        }
    };

    TokenStream::from(quote! {
        #expanded
    })
}

/// Generates the struct definition with proper syntax for different struct kinds.
fn generate_struct_definition(
    attrs: &[syn::Attribute],
    vis: &syn::Visibility,
    struct_name: &syn::Ident,
    generics: &syn::Generics,
    fields: &syn::Fields,
    semi: &Option<syn::token::Semi>,
) -> proc_macro2::TokenStream {
    if semi.is_some() {
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
    }
}

/// Generates extra traits (Deref, Display) for single-element tuple structs.
fn generate_extra_traits(
    tuple_info: &Option<TupleStructInfo>,
    struct_name: &syn::Ident,
) -> proc_macro2::TokenStream {
    if let Some(info) = tuple_info {
        let inner = &info.inner_type;
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
    tuple_info: &Option<TupleStructInfo>,
    provider_attrs: &ProviderAttrs,
    struct_name: &syn::Ident,
) -> proc_macro2::TokenStream {
    let Some(info) = tuple_info else {
        return quote! {};
    };

    // Helper to generate the to_owned() expansion if needed
    let dot_owned_expansion = |val: &String| {
        if info.is_string && val.starts_with('"') && val.ends_with('"') && !val.contains(".to_") {
            // It's a bare string literal for a String field - add .to_owned()
            format!("{}.to_owned()", val)
                .parse()
                .unwrap_or_else(|_| quote! { Default::default() })
        } else {
            val.parse()
                .unwrap_or_else(|_| quote! { Default::default() })
        }
    };

    // Build the default expression
    let struct_name_str = struct_name.to_string();
    let default_expr = if let Some(ref env_name) = provider_attrs.env_name {
        // Use env var with fallback to default
        if let Some(ref default_val) = provider_attrs.default_value {
            let default_tokens = dot_owned_expansion(default_val);
            quote! {
                std::env::var(#env_name).unwrap_or_else(|_| #default_tokens)
            }
        } else {
            // No fallback: env var is REQUIRED. Panic with a descriptive message
            // that includes both the env var name and the provider type so
            // operators can quickly locate the missing configuration.
            quote! {
                std::env::var(#env_name).unwrap_or_else(|_| {
                    panic!(
                        "Required environment variable '{}' is not set (needed by provider '{}'). \
                         Set it or add a default: #[provider(env_name = \"{}\", default = \"...\")]",
                        #env_name, #struct_name_str, #env_name
                    )
                })
            }
        }
    } else if let Some(ref default_val) = provider_attrs.default_value {
        let default_tokens = dot_owned_expansion(default_val);
        quote! { #default_tokens }
    } else {
        // No default specified, skip Default impl
        return quote! {};
    };

    quote! {
        impl Default for #struct_name {
            fn default() -> Self {
                Self(#default_expr)
            }
        }
    }
}

/// Generates the constructor for the struct provider.
fn generate_constructor(fields: &syn::Fields) -> proc_macro2::TokenStream {
    match fields {
        syn::Fields::Named(named_fields) => {
            let field_inits: Vec<_> = named_fields
                .named
                .iter()
                .map(|field| {
                    let field_name = field.ident.as_ref().expect("Named fields must have idents");
                    let field_type = &field.ty;

                    // Check if it's Arc<T>
                    if let Type::Path(syn::TypePath { path, .. }) = field_type
                        && let (Some(segment), true) =
                            (path.segments.last(), path.segments.len() == 1)
                        && segment.ident == "Arc"
                        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                        && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
                    {
                        // Async resolution with .await
                        quote! {
                            #field_name: <#inner_type as service_daemon::Provided>::resolve().await
                        }
                    } else {
                        // For non-Arc fields, use Default
                        quote! {
                            #field_name: Default::default()
                        }
                    }
                })
                .collect();

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

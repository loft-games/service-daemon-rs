//! Struct provider generation.
//!
//! This module generates providers for structs with automatic field injection.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemStruct, Type};

use super::parser::ProviderArgs;
use super::templates::{generate_broadcast_queue_template, generate_notify_template};

/// Attempts to generate a template-based provider if the args specify a known template.
/// Returns `Some(TokenStream)` if a template was matched, `None` otherwise.
fn try_generate_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    provider_args: &ProviderArgs,
) -> Option<TokenStream> {
    let ProviderArgs::Template {
        name,
        inner_type,
        capacity,
    } = provider_args
    else {
        return None;
    };

    match name.to_string().as_str() {
        // Signal templates
        "Notify" | "Event" => Some(generate_notify_template(struct_name, vis, attrs)),
        // Broadcast queue templates (fanout - all handlers receive the event)
        "BroadcastQueue" | "Queue" | "BQueue" => {
            let item_type = inner_type
                .clone()
                .unwrap_or_else(|| syn::parse_quote!(String));
            let cap = capacity.unwrap_or(100);
            Some(generate_broadcast_queue_template(
                struct_name,
                vis,
                attrs,
                &item_type,
                cap,
            ))
        }
        _ => {
            // Unknown template name — emit helpful error at the exact span
            proc_macro_error2::abort!(
                name,
                "Unknown provider template '{}'", name;
                help = "Supported templates: Notify, Event, Queue, BQueue, BroadcastQueue"
            );
        }
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

    /// Checks whether the type is `String` or `std::string::String`.
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
pub fn generate_struct_provider(item: ItemStruct, args: ProviderArgs) -> TokenStream {
    let struct_name = &item.ident;
    let vis = &item.vis;
    let attrs = &item.attrs;
    let generics = &item.generics;
    let fields = &item.fields;
    let semi = &item.semi_token;

    // Check for magic template defaults first
    if let Some(template_output) = try_generate_template(struct_name, vis, attrs, &args) {
        return template_output;
    }

    // Generate struct definition with proper syntax
    let struct_def = generate_struct_definition(attrs, vis, struct_name, generics, fields, semi);

    // Analyze tuple struct properties
    let tuple_info = TupleStructInfo::from_fields(fields);

    // Generate extra traits ONLY for single-element tuple structs
    let extra_traits = generate_extra_traits(&tuple_info, struct_name);

    // Generate Default impl for single-element tuple structs
    let default_impl = generate_default_impl(&tuple_info, &args, struct_name);

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
    provider_args: &ProviderArgs,
    struct_name: &syn::Ident,
) -> proc_macro2::TokenStream {
    let Some(info) = tuple_info else {
        return quote! {};
    };

    // Extract default value expression and optional env from args
    let (default_expr_opt, env_opt) = match provider_args {
        ProviderArgs::Value {
            default_value, env, ..
        } => {
            // Detect the empty-string sentinel from env-only parsing:
            // `#[provider(env = "KEY")]` produces default_value = `""` as a placeholder.
            // We treat this as "no default value" so the env-required panic path fires.
            let is_env_only_sentinel = matches!(
                default_value,
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) if s.value().is_empty()
            ) && env.is_some();

            let effective_default = if is_env_only_sentinel {
                None
            } else {
                Some(default_value)
            };
            (effective_default, env.as_ref())
        }
        _ => (None, None),
    };

    // Helper to wrap string literals with .to_owned() for String fields
    let expand_value = |expr: &syn::Expr| -> proc_macro2::TokenStream {
        if info.is_string {
            // Check if the expression is a bare string literal without .to_owned() etc.
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            }) = expr
            {
                return quote! { #expr.to_owned() };
            }
        }
        quote! { #expr }
    };

    // Build the default expression
    let struct_name_str = struct_name.to_string();
    let default_body = if let Some(env_lit) = env_opt {
        // Use env var with fallback to default
        let env_str = env_lit.value();
        if let Some(default_val) = default_expr_opt {
            let default_tokens = expand_value(default_val);
            quote! {
                std::env::var(#env_str).unwrap_or_else(|_| #default_tokens)
            }
        } else {
            // No fallback: env var is REQUIRED. Panic with a descriptive message.
            quote! {
                std::env::var(#env_str).unwrap_or_else(|_| {
                    panic!(
                        "Required environment variable '{}' is not set (needed by provider '{}'). \
                         Set it or add a default: #[provider(\"...\", env = \"{}\")]",
                        #env_str, #struct_name_str, #env_str
                    )
                })
            }
        }
    } else if let Some(default_val) = default_expr_opt {
        let default_tokens = expand_value(default_val);
        quote! { #default_tokens }
    } else {
        // No default specified, skip Default impl
        return quote! {};
    };

    quote! {
        impl Default for #struct_name {
            fn default() -> Self {
                Self(#default_body)
            }
        }
    }
}

/// Extracts the inner type `U` from a nested `Arc<Wrapper<U>>` pattern,
/// where `Wrapper` matches the given identifier (e.g., "RwLock" or "Mutex").
fn extract_arc_inner_wrapper<'a>(
    field_type: &'a Type,
    wrapper_name: &str,
) -> Option<&'a syn::Type> {
    // Match Arc<...>
    let syn::Type::Path(syn::TypePath { path, .. }) = field_type else {
        return None;
    };
    let (Some(arc_seg), true) = (path.segments.last(), path.segments.len() == 1) else {
        return None;
    };
    if arc_seg.ident != "Arc" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arc_args) = &arc_seg.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner)) = arc_args.args.first() else {
        return None;
    };

    // Match Wrapper<U> inside the Arc
    let syn::Type::Path(syn::TypePath {
        path: inner_path, ..
    }) = inner
    else {
        return None;
    };
    let (Some(wrapper_seg), true) = (inner_path.segments.last(), inner_path.segments.len() == 1)
    else {
        return None;
    };
    if wrapper_seg.ident != wrapper_name {
        return None;
    }
    let syn::PathArguments::AngleBracketed(wrapper_args) = &wrapper_seg.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(innermost)) = wrapper_args.args.first() else {
        return None;
    };

    Some(innermost)
}

/// Generates the constructor for the struct provider.
///
/// Supports automatic injection for:
/// - `Arc<T>` fields → `<T as Provided>::resolve().await`
/// - `Arc<RwLock<T>>` fields → `<T as Provided>::resolve_rwlock().await`
/// - `Arc<Mutex<T>>` fields → `<T as Provided>::resolve_mutex().await`
/// - Other fields → `Default::default()`
fn generate_constructor(fields: &syn::Fields) -> proc_macro2::TokenStream {
    match fields {
        syn::Fields::Named(named_fields) => {
            let field_inits: Vec<_> = named_fields
                .named
                .iter()
                .map(|field| {
                    let field_name = field.ident.as_ref().expect("Named fields must have idents");
                    let field_type = &field.ty;

                    // Check for Arc<RwLock<T>> → resolve_rwlock()
                    if let Some(inner_type) = extract_arc_inner_wrapper(field_type, "RwLock") {
                        return quote! {
                            #field_name: <#inner_type as service_daemon::Provided>::resolve_rwlock().await
                        };
                    }

                    // Check for Arc<Mutex<T>> → resolve_mutex()
                    if let Some(inner_type) = extract_arc_inner_wrapper(field_type, "Mutex") {
                        return quote! {
                            #field_name: <#inner_type as service_daemon::Provided>::resolve_mutex().await
                        };
                    }

                    // Check if it's Arc<T> → resolve()
                    if let Type::Path(syn::TypePath { path, .. }) = field_type
                        && let (Some(segment), true) =
                            (path.segments.last(), path.segments.len() == 1)
                        && segment.ident == "Arc"
                        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                        && let Some(syn::GenericArgument::Type(inner_type)) = args.args.first()
                    {
                        // Async resolution with .await
                        return quote! {
                            #field_name: <#inner_type as service_daemon::Provided>::resolve().await
                        };
                    }

                    // For non-Arc fields, use Default
                    quote! {
                        #field_name: Default::default()
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

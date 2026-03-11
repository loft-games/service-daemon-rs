//! Struct provider generation.
//!
//! This module generates providers for structs with automatic field injection.

use proc_macro::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::ItemStruct;
use syn::spanned::Spanned;

use super::parser::{ProviderArgs, ProviderKind, TemplateArg};
use super::templates::{
    generate_broadcast_queue_template, generate_listen_template, generate_notify_template,
};

/// Attempts to generate a template-based provider if the args specify a known template.
/// Returns `Some(TokenStream)` if a template was matched, `None` otherwise.
///
/// Also emits warnings for unused named arguments (e.g., `capacity` on Notify).
fn try_generate_template(
    struct_name: &syn::Ident,
    vis: &syn::Visibility,
    attrs: &[syn::Attribute],
    provider_args: &ProviderArgs,
) -> Option<TokenStream> {
    let ProviderKind::Template { name, arg } = &provider_args.kind else {
        return None;
    };

    match name.to_string().as_str() {
        // Signal templates — no named arguments are useful
        "Notify" | "Event" => {
            if provider_args.env.is_some() {
                proc_macro_error2::emit_warning!(
                    name,
                    "Notify/Event template does not use `env`; it will be ignored"
                );
            }
            if provider_args.capacity.is_some() {
                proc_macro_error2::emit_warning!(
                    name,
                    "Notify/Event template does not use `capacity`; it will be ignored"
                );
            }
            Some(generate_notify_template(struct_name, vis, attrs))
        }
        // Broadcast queue templates (fanout - all handlers receive the event)
        "BroadcastQueue" | "Queue" | "BQueue" => {
            let item_type = match arg {
                Some(TemplateArg::Type(ty)) => (*ty).clone(),
                _ => syn::parse_quote!(String),
            };
            let cap = provider_args.capacity.unwrap_or(100);
            if provider_args.env.is_some() {
                proc_macro_error2::emit_warning!(
                    name,
                    "Queue template does not use `env`; it will be ignored"
                );
            }
            Some(generate_broadcast_queue_template(
                struct_name,
                vis,
                attrs,
                &item_type,
                cap,
            ))
        }
        // Listen template (TCP listener with FD cloning)
        "Listen" => {
            let bind_addr = match arg {
                Some(TemplateArg::Addr(lit)) => lit,
                _ => {
                    proc_macro_error2::abort!(
                        name,
                        "Listen template requires a bind address";
                        help = r#"Usage: #[provider(Listen("0.0.0.0:8080"))]"#
                    );
                }
            };
            if provider_args.capacity.is_some() {
                proc_macro_error2::emit_warning!(
                    name,
                    "Listen template does not use `capacity`; it will be ignored"
                );
            }
            Some(generate_listen_template(
                struct_name,
                vis,
                attrs,
                bind_addr,
                provider_args.env.as_ref(),
            ))
        }
        _ => {
            // Unknown template name — emit helpful error at the exact span
            proc_macro_error2::abort!(
                name,
                "Unknown provider template '{}'", name;
                help = "Supported templates: Notify, Event, Queue, BQueue, BroadcastQueue, Listen"
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

/// Generates the provider capability trait impls and convenience methods
/// for a provider type, and registers a `ProviderEntry` in the
/// `PROVIDER_REGISTRY` for dependency graph analysis.
///
/// This shared function eliminates the code duplication that previously existed
/// across struct providers, fn providers, and template providers.
///
/// # Arguments
/// * `type_tokens` — The type that implements provider traits (as a token stream).
/// * `singleton_name` — The unique static `StateManager` identifier.
/// * `constructor` — The expression to create `Arc<Self>` on first resolution.
/// * `user_span`  — The span of the user's type definition (struct name or fn
///   return type). Used for `quote_spanned!` so that missing trait bound errors
///   (e.g., `Clone`) point to the user's code, not the macro output.
/// * `param_entries` — Dependency metadata tokens for `PROVIDER_REGISTRY` registration.
pub(super) fn generate_provided_impl(
    type_tokens: &proc_macro2::TokenStream,
    singleton_name: &syn::Ident,
    constructor: &proc_macro2::TokenStream,
    user_span: proc_macro2::Span,
    param_entries: &[proc_macro2::TokenStream],
) -> proc_macro2::TokenStream {
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
        impl service_daemon::WatchableProvided for #type_tokens {
            async fn changed() {
                #singleton_name.changed().await
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

    quote! {
        #bounds_assertion

        static #singleton_name: service_daemon::core::managed_state::StateManager<#type_tokens> = service_daemon::core::managed_state::StateManager::new();

        impl service_daemon::Provided for #type_tokens {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async {
                    #constructor
                }).await
            }
        }

        impl service_daemon::ManagedProvided for #type_tokens {
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
        }

        #watchable_impl

        impl #type_tokens {
            /// Resolves a tracked RwLock for this provider.
            pub async fn rwlock(&self) -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                <Self as service_daemon::ManagedProvided>::resolve_rwlock().await
            }

            /// Resolves a tracked Mutex for this provider.
            pub async fn mutex(&self) -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
                <Self as service_daemon::ManagedProvided>::resolve_mutex().await
            }
        }

        /// Auto-generated provider registry entry for dependency graph analysis.
        #[allow(unsafe_code)] // linkme uses #[link_section] internally
        #[service_daemon::linkme::distributed_slice(service_daemon::PROVIDER_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ProviderEntry = service_daemon::ProviderEntry {
            name: #type_name_str,
            module: module_path!(),
            type_id: std::any::TypeId::of::<#type_tokens>(),
            params: &[#(#param_entries),*],
        };
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

    // Collect dependency metadata from struct fields for PROVIDER_REGISTRY.
    // Only named fields with Arc-wrapped types are injectable dependencies.
    let param_entries: Vec<proc_macro2::TokenStream> = match fields {
        syn::Fields::Named(named_fields) => named_fields
            .named
            .iter()
            .filter_map(|field| {
                let field_name = field.ident.as_ref()?;
                let (inner_type, wrapper) = crate::common::decompose_type(&field.ty);
                // Only Arc-wrapped fields are DI dependencies
                wrapper.map(|_| {
                    let field_name_str = field_name.to_string();
                    let type_str = quote!(#inner_type).to_string().replace(' ', "");
                    quote! {
                        service_daemon::ServiceParam {
                            name: #field_name_str,
                            type_name: #type_str,
                            type_id: std::any::TypeId::of::<#inner_type>(),
                        }
                    }
                })
            })
            .collect(),
        _ => Vec::new(),
    };

    // Generate unique static name for singleton.
    //
    // Safety: Rust's `static` items are scoped to the enclosing module, so
    // two structs with the same name in different modules produce separate
    // statics. Within a single module, Rust forbids duplicate type names,
    // making name collisions impossible under normal usage.
    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        struct_name.to_string().to_uppercase()
    );

    // Use the shared Provided impl generator (Fix #1)
    let type_tokens = quote! { #struct_name };
    let provided_impl = generate_provided_impl(
        &type_tokens,
        &singleton_name,
        &constructor,
        struct_name.span(),
        &param_entries,
    );

    let expanded = quote! {
        #struct_def

        #extra_traits

        #default_impl

        #provided_impl
    };

    TokenStream::from(expanded)
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

        // Build Display impl with quote_spanned! so that if the inner type
        // doesn't implement Display, the error points to the user's type (R2).
        let inner_span = inner.span();
        let display_impl = quote_spanned! { inner_span =>
            impl std::fmt::Display for #struct_name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    std::fmt::Display::fmt(&self.0, f)
                }
            }
        };

        quote! {
            impl std::ops::Deref for #struct_name {
                type Target = #inner;
                fn deref(&self) -> &#inner {
                    &self.0
                }
            }

            #display_impl
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

    // Extract default value expression from args.
    let default_expr_opt = match &provider_args.kind {
        ProviderKind::Value { default_value } => default_value.as_ref(),
        _ => None,
    };
    // Read env from the shared field.
    let env_opt = provider_args.env.as_ref();

    // Capacity on Value providers is semantically invalid — emit error.
    if provider_args.capacity.is_some() {
        proc_macro_error2::emit_error!(
            struct_name,
            "`capacity` is not supported on value providers; use a template like Queue instead"
        );
    }

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

        if info.is_string {
            // String type: use env var directly without parsing
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
        } else {
            // Non-String type: parse the env var string into the target type.
            // This enables `#[provider(8080, env = "PORT")] struct Port(pub i32)`.
            let inner_ty = &info.inner_type;
            if let Some(default_val) = default_expr_opt {
                let default_tokens = expand_value(default_val);
                quote! {
                    std::env::var(#env_str)
                        .ok()
                        .and_then(|v| v.parse::<#inner_ty>().ok())
                        .unwrap_or_else(|| #default_tokens)
                }
            } else {
                // No fallback: env var is REQUIRED and must be parseable.
                quote! {
                    std::env::var(#env_str)
                        .unwrap_or_else(|_| {
                            panic!(
                                "Required environment variable '{}' is not set (needed by provider '{}'). \
                                 Set it or add a default: #[provider(value, env = \"{}\")]",
                                #env_str, #struct_name_str, #env_str
                            )
                        })
                        .parse::<#inner_ty>()
                        .unwrap_or_else(|e| {
                            panic!(
                                "Environment variable '{}' for provider '{}' cannot be parsed: {}",
                                #env_str, #struct_name_str, e
                            )
                        })
                }
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

/// Generates the constructor for the struct provider.
///
/// Supports automatic injection for:
/// - `Arc<T>` fields -> `<T as Provided>::resolve().await`
/// - `Arc<RwLock<T>>` fields -> `<T as ManagedProvided>::resolve_rwlock().await`
/// - `Arc<Mutex<T>>` fields -> `<T as ManagedProvided>::resolve_mutex().await`
/// - Other fields -> `Default::default()`
///
/// Uses `decompose_type` from `common` to avoid duplicating Arc pattern matching (Fix #8).
fn generate_constructor(fields: &syn::Fields) -> proc_macro2::TokenStream {
    match fields {
        syn::Fields::Named(named_fields) => {
            let field_inits: Vec<_> = named_fields
                .named
                .iter()
                .map(|field| {
                    let field_name = field.ident.as_ref().expect("Named fields must have idents");
                    let field_type = &field.ty;

                    // Use the shared type decomposition from common.rs
                    let (inner_type, wrapper) = crate::common::decompose_type(field_type);

                    match wrapper {
                        Some(crate::common::WrapperKind::ArcRwLock(_, _)) => {
                            quote! {
                                #field_name: <#inner_type as service_daemon::ManagedProvided>::resolve_rwlock().await
                            }
                        }
                        Some(crate::common::WrapperKind::ArcMutex(_, _)) => {
                            quote! {
                                #field_name: <#inner_type as service_daemon::ManagedProvided>::resolve_mutex().await
                            }
                        }
                        Some(crate::common::WrapperKind::Arc(_)) => {
                            quote! {
                                #field_name: <#inner_type as service_daemon::Provided>::resolve().await
                            }
                        }
                        None => {
                            // For non-Arc fields, use Default.
                            // Use quote_spanned! to direct compile errors to the
                            // field declaration rather than obscure macro output.
                            //
                            // The explicit trait-call style `<Type as Default>::default()`
                            // combined with quote_spanned! produces clear diagnostics
                            // that point directly to the user's field type when Default
                            // is not implemented.
                            let field_span = field_type.span();
                            quote_spanned! { field_span =>
                                #field_name: <#field_type as Default>::default()
                            }
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

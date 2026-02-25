//! `#[provider]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `parser`: Attribute parsing and configuration.
//! - `templates`: Template generators for Notify, Queue, LBQueue.
//! - `struct_gen`: Struct provider generation with field injection.

mod parser;
mod struct_gen;
mod templates;

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::ItemFn;

use crate::common::has_allow_sync;
use parser::parse_provider_attrs;
use struct_gen::generate_struct_provider;

/// Implementation of the `#[provider]` attribute macro.
pub fn provider_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_str = attr.to_string();

    // Support struct items for type-based DI
    if let Ok(item_struct) = syn::parse::<syn::ItemStruct>(item.clone()) {
        return generate_struct_provider(item_struct, &attr_str);
    }

    // Support async fn items for custom async initialization
    if let Ok(item_fn) = syn::parse::<ItemFn>(item.clone()) {
        return generate_async_fn_provider(item_fn, parse_provider_attrs(&attr_str));
    }

    // Error for unsupported items - use abort! for enhanced error
    abort!(
        proc_macro2::TokenStream::from(item),
        "#[provider] can only be applied to struct or async fn items";
        help = "Use #[provider] on a struct definition or an async function";
        note = "Example: #[provider(default = 8080)] pub struct Port(pub i32);"
    )
}

/// Generates a provider from an async function.
fn generate_async_fn_provider(
    item_fn: ItemFn,
    _provider_attrs: parser::ProviderAttrs,
) -> TokenStream {
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
    let return_type_str = quote!(#return_type).to_string().replace(" ", "");
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

        static #singleton_name: service_daemon::core::managed_state::StateManager<#return_type> = service_daemon::core::managed_state::StateManager::new();

        // Generate Provided impl for the return type using StateManager
        impl service_daemon::Provided for #return_type {
            async fn resolve() -> std::sync::Arc<Self> {
                #singleton_name.resolve_snapshot(|| async {
                    std::sync::Arc::new(#call_expr)
                }).await
            }

            async fn resolve_rwlock() -> std::sync::Arc<service_daemon::core::managed_state::RwLock<Self>> {
                #singleton_name.resolve_rwlock(|| async {
                    std::sync::Arc::new(#call_expr)
                }).await
            }

            async fn resolve_mutex() -> std::sync::Arc<service_daemon::core::managed_state::Mutex<Self>> {
                #singleton_name.resolve_mutex(|| async {
                    std::sync::Arc::new(#call_expr)
                }).await
            }

            async fn changed() {
                #singleton_name.changed().await
            }
        }

        impl #return_type {
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

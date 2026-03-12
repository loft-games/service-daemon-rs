//! `#[provider]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `parser`: Attribute parsing and configuration.
//! - `templates`: Template generators for Notify, Queue.
//! - `struct_gen`: Struct provider generation with field injection.

mod parser;
mod struct_gen;
mod templates;

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{ItemFn, parse_macro_input};

use crate::common::extract_sync_handler_flag;
pub use parser::ProviderArgs;
use struct_gen::generate_struct_provider;

fn extract_fallible_provider_type(ty: &syn::Type) -> Option<syn::Type> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };

    let last = tp.path.segments.last()?;
    if last.ident != "Result" {
        return None;
    }

    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };

    // Expect Result<Ok, Err>
    let mut iter = args.args.iter();
    let ok = iter.next()?;
    let err = iter.next()?;

    let ok_ty = match ok {
        syn::GenericArgument::Type(t) => t.clone(),
        _ => return None,
    };
    let err_ty = match err {
        syn::GenericArgument::Type(t) => t,
        _ => return None,
    };

    // Only accept error type whose last segment ident is ProviderError
    if let syn::Type::Path(err_path) = err_ty {
        if err_path
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "ProviderError")
        {
            return Some(ok_ty);
        }
    }

    None
}

/// Implementation of the `#[provider]` attribute macro.
///
/// Uses a peek-first-token strategy to determine the item kind before parsing,
/// avoiding unnecessary `TokenStream::clone()` and trial-error parsing.
pub fn provider_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Peek at the first meaningful tokens to determine the item kind.
    // This avoids cloning the full TokenStream for trial parsing.
    //
    // Safety assumption: In attribute macro context, `item` does NOT include
    // other `#[...]` attributes (e.g., `#[derive(...)]`, `#[cfg(...)]`).
    // The compiler strips them before passing the token stream to us.
    // Therefore, the first idents are guaranteed to be visibility qualifiers
    // and/or the item keyword (e.g., `pub`, `struct`, `async`, `fn`).
    let item2: proc_macro2::TokenStream = item.clone().into();
    let first_tokens: Vec<String> = item2
        .into_iter()
        .filter_map(|tt| {
            if let proc_macro2::TokenTree::Ident(ident) = tt {
                Some(ident.to_string())
            } else {
                None
            }
        })
        .take(3) // e.g., ["pub", "async", "fn"] or ["pub", "struct", "Name"]
        .collect();

    let has_struct = first_tokens.iter().any(|t| t == "struct");
    let has_fn = first_tokens.iter().any(|t| t == "fn");

    if has_struct {
        let item_struct = parse_macro_input!(item as syn::ItemStruct);
        let args = parse_macro_input!(attr as ProviderArgs);
        return generate_struct_provider(item_struct, args);
    }

    if has_fn {
        let item_fn = parse_macro_input!(item as ItemFn);
        if !attr.is_empty() {
            proc_macro_error2::emit_warning!(
                proc_macro2::TokenStream::from(attr),
                "#[provider] on fn ignores arguments";
                help = "Use `#[provider]` without arguments for fn providers"
            );
        }
        return generate_async_fn_provider(item_fn);
    }

    // Error for unsupported items - use abort! for enhanced error
    abort!(
        proc_macro2::TokenStream::from(item),
        "#[provider] can only be applied to struct or async fn items";
        help = "Use #[provider] on a struct definition or an async function";
        note = "Example: #[provider(8080)] pub struct Port(pub i32);"
    )
}

/// Generates a provider from an async function with automatic dependency injection.
///
/// Supports the same `Arc<T>` injection patterns as `#[service]` / `#[trigger]`:
/// - `Arc<T>` -> `<T as Provided>::resolve().await`
/// - `Arc<RwLock<T>>` -> `<T as ManagedProvided>::resolve_rwlock().await`
/// - `Arc<Mutex<T>>` -> `<T as ManagedProvided>::resolve_mutex().await`
///
/// The function is preserved as-is with its original signature. The generated
/// `Provided` impl calls the function with resolved dependencies.
fn generate_async_fn_provider(item_fn: ItemFn) -> TokenStream {
    let fn_name = &item_fn.sig.ident;
    let fn_vis = &item_fn.vis;
    let fn_block = &item_fn.block;
    let fn_asyncness = &item_fn.sig.asyncness;
    let fn_inputs = &item_fn.sig.inputs;

    // Extract return type
    let return_type = match &item_fn.sig.output {
        syn::ReturnType::Type(_, ty) => ty.clone(),
        syn::ReturnType::Default => {
            abort!(
                &item_fn.sig,
                "#[provider] fn must have a return type";
                help = "Add a return type, e.g., `async fn config() -> MyConfig { ... }`"
            );
        }
    };

    // Detect fallible provider: Result<T, ProviderError>
    let (provided_type, is_fallible) = match extract_fallible_provider_type(&return_type) {
        Some(inner) => (inner, true),
        None => ((*return_type).clone(), false),
    };

    let fn_name_str = fn_name.to_string();
    let return_type_str = quote!(#provided_type).to_string().replace(" ", "");
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&item_fn.attrs);

    // Process function parameters for DI resolution
    let mut resolve_tokens = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();

    for arg in fn_inputs {
        // Reject `self` / `&self` / `&mut self` — providers must be free functions.
        if let syn::FnArg::Receiver(_) = arg {
            abort!(
                arg,
                "#[provider] fn must be a free function, not a method";
                help = "Remove the `self` parameter"
            );
        }

        if let syn::FnArg::Typed(syn::PatType { pat, ty, .. }) = arg
            && let syn::Pat::Ident(pat_ident) = &**pat
        {
            let arg_name = &pat_ident.ident;
            let (inner_type, wrapper) = crate::common::decompose_type(ty);

            match wrapper {
                Some(crate::common::WrapperKind::ArcRwLock(_, _)) => {
                    resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_rwlock().await;
                    });
                }
                Some(crate::common::WrapperKind::ArcMutex(_, _)) => {
                    resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_mutex().await;
                    });
                }
                Some(crate::common::WrapperKind::Arc(_)) => {
                    resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                    });
                }
                None => {
                    abort!(
                        arg,
                        "Provider function parameters must be Arc-wrapped dependencies";
                        help = "Use Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>"
                    );
                }
            }

            // Collect dependency metadata for PROVIDER_REGISTRY
            let arg_name_str = arg_name.to_string();
            let type_str = quote!(#inner_type).to_string().replace(' ', "");
            param_entries.push(quote! {
                service_daemon::ServiceParam {
                    name: #arg_name_str,
                    type_name: #type_str,
                    type_id: std::any::TypeId::of::<#inner_type>(),
                }
            });

            call_args.push(quote! { #arg_name });
        }
    }

    // Build the call expression with or without dependency injection
    let fn_call_with_args = if fn_asyncness.is_some() {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        // User explicitly allowed sync, no warning
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Provider function '{}' for type '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str, #return_type_str);
                #fn_name(#(#call_args),*)
            }
        }
    };

    // Singleton name uses the function name (unique within a module)
    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        fn_name.to_string().to_uppercase()
    );

    // Constructor resolves dependencies then calls the function
    let constructor = if is_fallible {
        quote! {
            #(#resolve_tokens)*
            service_daemon::core::provider_init::init_fallible(
                service_daemon::RestartPolicy::default(),
                service_daemon::tokio_util::sync::CancellationToken::new(),
                || async { #fn_call_with_args },
            )
            .await
        }
    } else {
        quote! {
            #(#resolve_tokens)*
            std::sync::Arc::new(#fn_call_with_args)
        }
    };

    // Use the shared Provided impl generator
    let type_tokens = quote! { #provided_type };
    let init_fn = if is_fallible {
        quote! {
            #(#resolve_tokens)*
            service_daemon::core::provider_init::init_fallible(
                policy,
                cancel,
                || async { #fn_call_with_args },
            )
            .await
        }
    } else {
        quote! {
            let _ = policy;
            let _ = cancel;
            #constructor
        }
    };

    let provided_impl = struct_gen::generate_provided_impl(
        &type_tokens,
        &singleton_name,
        return_type.span(),
        &param_entries,
        false,
        &init_fn,
    );

    let expanded = quote! {
        // Keep the original function with its full signature and attributes preserved
        #(#cleaned_attrs)*
        #fn_vis #fn_asyncness fn #fn_name(#fn_inputs) -> #return_type
            #fn_block

        #provided_impl
    };

    TokenStream::from(expanded)
}

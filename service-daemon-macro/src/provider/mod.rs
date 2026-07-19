//! `#[provider]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `parser`: Attribute parsing and configuration.
//! - `templates`: Template generators for Notify, Queue.
//! - `struct_gen`: Struct provider generation with field injection.

mod impls;
mod parser;
mod struct_gen;
mod templates;

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Item, ItemFn, parse_macro_input};

use crate::common::{WrapperKind, decompose_type, extract_sync_handler_flag};
use impls::{HelperStyle, ProvidedImplConfig, generate_provided_impl};
pub use parser::ProviderArgs;
use struct_gen::generate_struct_provider;

struct FallibleProviderReturn {
    ok_ty: syn::Type,
    err_ty: syn::Type,
}

fn extract_result_provider_return(ty: &syn::Type) -> Option<FallibleProviderReturn> {
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

    let mut iter = args.args.iter();
    let ok = iter.next()?;
    let err = iter.next()?;

    let ok_ty = match ok {
        syn::GenericArgument::Type(t) => t.clone(),
        _ => return None,
    };
    let err_ty = match err {
        syn::GenericArgument::Type(t) => t.clone(),
        _ => return None,
    };

    Some(FallibleProviderReturn { ok_ty, err_ty })
}

fn is_provider_error_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };

    tp.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "ProviderError")
}

fn provider_error_type_assertion(err_ty: &syn::Type) -> proc_macro2::TokenStream {
    let span = err_ty.span();
    quote_spanned! { span =>
        const _: () = {
            fn __service_daemon_assert_provider_error_type(value: #err_ty) {
                let _: service_daemon::ProviderError = value;
            }
        };
    }
}

pub fn provider_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed_item = parse_macro_input!(item as Item);
    let args = parse_macro_input!(attr as ProviderArgs);

    match parsed_item {
        Item::Struct(item_struct) => generate_struct_provider(item_struct, args),
        Item::Fn(item_fn) => generate_async_fn_provider(item_fn, args.eager),
        other => abort!(
            other,
            "#[provider] can only be applied to struct or function items";
            help = "Use #[provider] on a struct definition or a function returning the provider type";
            note = "Example: #[provider(8080)] pub struct Port(pub i32);"
        ),
    }
}

fn generate_async_fn_provider(item_fn: ItemFn, eager: bool) -> TokenStream {
    let fn_name = &item_fn.sig.ident;
    let fn_vis = &item_fn.vis;
    let fn_sig = &item_fn.sig;
    let fn_block = &item_fn.block;
    let fn_asyncness = &item_fn.sig.asyncness;
    let fn_inputs = &item_fn.sig.inputs;

    if let syn::Safety::Unsafe(unsafety) = &item_fn.sig.safety {
        abort!(
            unsafety,
            "#[provider] fn cannot be unsafe";
            help = "Move unsafe operations behind a safe provider function boundary"
        );
    }

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

    let result_return = extract_result_provider_return(&return_type);
    if let Some(result_return) = &result_return
        && !is_provider_error_type(&result_return.err_ty)
    {
        abort!(
            result_return.err_ty,
            "#[provider] function Result error type must be service_daemon::ProviderError";
            help = "Use Result<T, service_daemon::ProviderError> for provider-init retry/fatal semantics, or wrap non-framework Result values in a local provider type"
        );
    }

    let fallible_return = result_return;
    let provider_error_assertion = fallible_return
        .as_ref()
        .map(|fallible| provider_error_type_assertion(&fallible.err_ty));
    let (provided_type, is_fallible) = match &fallible_return {
        Some(fallible) => (fallible.ok_ty.clone(), true),
        None => ((*return_type).clone(), false),
    };

    let fn_name_str = fn_name.to_string();
    let return_type_str = quote!(#provided_type).to_string().replace(" ", "");
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&item_fn.attrs);

    let mut framework_resolve_tokens = Vec::new();
    let mut managed_resolve_tokens = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();

    for arg in fn_inputs {
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
            let (inner_type, wrapper) = decompose_type(ty);

            match wrapper {
                Some(WrapperKind::ArcRwLock(_, _)) => {
                    framework_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_rwlock()
                            .await
                            .map_err(|e| service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::DependencyProvider,
                                e,
                            ))?;
                    });
                    managed_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_rwlock()
                            .await
                            .map_err(|e| service_daemon::ProviderError::Fatal(e.to_string()))?;
                    });
                }
                Some(WrapperKind::ArcMutex(_, _)) => {
                    framework_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_mutex()
                            .await
                            .map_err(|e| service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::DependencyProvider,
                                e,
                            ))?;
                    });
                    managed_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_mutex()
                            .await
                            .map_err(|e| service_daemon::ProviderError::Fatal(e.to_string()))?;
                    });
                }
                Some(WrapperKind::Arc(_)) => {
                    framework_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve()
                            .await
                            .map_err(|e| service_daemon::__private::ProviderInitFailure::new(
                                service_daemon::__private::ProviderInitSourceKind::DependencyProvider,
                                e,
                            ))?;
                    });
                    managed_resolve_tokens.push(quote! {
                        let #arg_name = <#inner_type as service_daemon::ManagedProvided>::resolve_managed().await?;
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

            let arg_name_str = arg_name.to_string();
            let type_str = quote!(#inner_type).to_string().replace(' ', "");
            param_entries.push(quote! {
                service_daemon::__private::ServiceParam {
                    name: #arg_name_str,
                    type_name: #type_str,
                    type_id: std::any::TypeId::of::<#inner_type>(),
                }
            });

            call_args.push(quote! { #arg_name });
        }
    }

    let fn_call_with_args = if fn_asyncness.is_some() {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Provider function '{}' for type '{}' is synchronous. Consider switching to 'async fn'.", #fn_name_str, #return_type_str);
                #fn_name(#(#call_args),*)
            }
        }
    };

    let singleton_name = format_ident!(
        "__PROVIDER_SINGLETON_{}",
        fn_name.to_string().to_uppercase()
    );

    let framework_init_fn = if is_fallible {
        quote! {
            #(#framework_resolve_tokens)*
            service_daemon::__private::init_fallible(
                #fn_name_str,
                policy,
                cancel,
                move || {
                    #(let #call_args = #call_args.clone();)*
                    async move { #fn_call_with_args }
                },
            )
            .await
        }
    } else {
        quote! {
            #(#framework_resolve_tokens)*
            let _ = policy;
            let _ = cancel;
            Ok(std::sync::Arc::new(#fn_call_with_args))
        }
    };

    let managed_init_fn = if is_fallible {
        quote! {
            #(#managed_resolve_tokens)*
            let value = #fn_call_with_args?;
            Ok(std::sync::Arc::new(value))
        }
    } else {
        quote! {
            #(#managed_resolve_tokens)*
            Ok(std::sync::Arc::new(#fn_call_with_args))
        }
    };

    let type_tokens = quote! { #provided_type };
    let helper_style = if is_fallible || !param_entries.is_empty() {
        HelperStyle::Fallible
    } else {
        HelperStyle::Infallible
    };
    let provider_origin = format!("#[provider] function {fn_name_str}");
    let provided_impl = generate_provided_impl(ProvidedImplConfig {
        type_tokens: &type_tokens,
        singleton_name: &singleton_name,
        user_span: return_type.span(),
        param_entries: &param_entries,
        eager,
        framework_init_fn: &framework_init_fn,
        managed_init_fn: &managed_init_fn,
        helper_style,
        provider_origin,
    });

    let expanded = quote! {
        #(#cleaned_attrs)*
        #fn_vis #fn_sig #fn_block

        #provider_error_assertion

        #provided_impl
    };

    TokenStream::from(expanded)
}

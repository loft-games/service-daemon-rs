//! `#[service]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `codegen`: Code generation for watchers and call expressions.

mod codegen;

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote, quote_spanned};
use syn::{ItemFn, parse_macro_input};

use crate::common::has_allow_sync;
use codegen::{generate_call_expr, generate_watcher};

/// Implementation of the `#[service]` attribute macro.
pub fn service_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_str = attr.to_string();
    let priority_expr =
        parse_service_attr(&attr_str, "priority").unwrap_or_else(|| "50".to_string());
    let priority_tokens: proc_macro2::TokenStream =
        priority_expr.parse().unwrap_or_else(|_| quote!(50));

    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;

    // Extract parameters and categorize them
    let (clean_inputs, resolve_tokens, call_args, param_entries, watcher_select_arms) =
        extract_params(sig);

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let wrapper_name = format_ident!("{}_wrapper", fn_name);
    let entry_name = format_ident!("__SERVICE_ENTRY_{}", fn_name.to_string().to_uppercase());

    let is_async = input.sig.asyncness.is_some();
    let allow_sync_present = has_allow_sync(attrs);
    let call_expr = generate_call_expr(
        fn_name,
        &fn_name_str,
        &call_args,
        is_async,
        allow_sync_present,
    );

    let (watcher_fn, watcher_ptr) = generate_watcher(fn_name, &watcher_select_arms);

    let expanded = quote! {
        #(#attrs)*
        #vis #clean_sig {
            // "Macro Illusion": Redirect RwLock/Mutex to our tracked versions
            #[allow(unused_imports)]
            use service_daemon::utils::managed_state::{RwLock, Mutex};
            #body
        }

        /// Auto-generated wrapper for the service - resolves dependencies via Type-Based DI
        pub fn #wrapper_name(token: service_daemon::tokio_util::sync::CancellationToken) -> service_daemon::futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #(#resolve_tokens)*
                #call_expr
            })
        }

        #watcher_fn

        /// Auto-generated static registry entry - collected by linkme at link time
        #[service_daemon::linkme::distributed_slice(service_daemon::SERVICE_REGISTRY)]
        #[linkme(crate = service_daemon::linkme)]
        static #entry_name: service_daemon::ServiceEntry = service_daemon::ServiceEntry {
            name: #fn_name_str,
            module: module_path!(),
            params: &[#(#param_entries),*],
            wrapper: #wrapper_name,
            watcher: #watcher_ptr,
            priority: #priority_tokens,
        };

    };

    TokenStream::from(expanded)
}

/// Extracts and categorizes parameters from the function signature.
///
/// Returns a tuple of:
/// - `clean_inputs`: Cleaned function inputs.
/// - `resolve_tokens`: Tokens for resolving dependencies.
/// - `call_args`: Arguments to pass to the user function.
/// - `param_entries`: ServiceParam entries for registry.
/// - `watcher_select_arms`: Select arms for the watcher function.
fn extract_params(
    sig: &syn::Signature,
) -> (
    syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>,
    Vec<proc_macro2::TokenStream>,
    Vec<proc_macro2::TokenStream>,
    Vec<proc_macro2::TokenStream>,
    Vec<proc_macro2::TokenStream>,
) {
    let mut resolve_tokens = Vec::new();
    let mut call_args = Vec::new();
    let mut param_entries = Vec::new();
    let mut watcher_select_arms = Vec::new();

    let mut clean_inputs = syn::punctuated::Punctuated::<syn::FnArg, syn::token::Comma>::new();
    for arg in &sig.inputs {
        if let Some((arg_name, intent)) = crate::common::analyze_param(arg) {
            let arg_name_str = arg_name.to_string();

            match intent {
                crate::common::ParamIntent::Dependency {
                    inner_type,
                    wrapper,
                } => {
                    let type_str = quote!(#inner_type).to_string().replace(" ", "");
                    let arg_type_wrapper_str = match wrapper {
                        crate::common::WrapperKind::Arc(_) => format!("Arc<{}>", type_str),
                        crate::common::WrapperKind::ArcRwLock(_, _) => {
                            format!("Arc<RwLock<{}>>", type_str)
                        }
                        crate::common::WrapperKind::ArcMutex(_, _) => {
                            format!("Arc<Mutex<{}>>", type_str)
                        }
                    };

                    match wrapper {
                        crate::common::WrapperKind::Arc(arc_span) => {
                            resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                            });
                            clean_inputs.push(
                                syn::parse2(quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#inner_type> })
                                    .unwrap(),
                            );
                        }
                        crate::common::WrapperKind::ArcRwLock(arc_span, rwlock_span) => {
                            resolve_tokens.push(quote! {
                                let #arg_name = #inner_type::rwlock().await;
                            });
                            let rw_path = quote_spanned! { rwlock_span => service_daemon::utils::managed_state::RwLock<#inner_type> };
                            clean_inputs.push(syn::parse2(quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#rw_path> }).unwrap());
                        }
                        crate::common::WrapperKind::ArcMutex(arc_span, mutex_span) => {
                            resolve_tokens.push(quote! {
                                let #arg_name = #inner_type::mutex().await;
                            });
                            let mutex_path = quote_spanned! { mutex_span => service_daemon::utils::managed_state::Mutex<#inner_type> };
                            clean_inputs.push(syn::parse2(quote_spanned! { arc_span => #arg_name: service_daemon::Arc<#mutex_path> }).unwrap());
                        }
                    }

                    call_args.push(quote! { #arg_name });
                    let key_str = format!("{}_{}", arg_name_str, arg_type_wrapper_str);
                    param_entries.push(quote! {
                        service_daemon::ServiceParam { name: #arg_name_str, type_name: #type_str, key: #key_str }
                    });

                    watcher_select_arms.push(quote! {
                        _ = <#inner_type as service_daemon::Provided>::changed() => {}
                    });
                }
                crate::common::ParamIntent::Payload { .. } => {
                    abort!(
                        arg,
                        "Services do not support event payloads. Only Arc<T> dependencies are allowed.";
                        help = "Remove the payload parameter or use #[trigger] instead."
                    );
                }
            }
            continue;
        }

        abort!(
            arg,
            "Unsupported parameter type. Service parameters must be Arc wrappers.";
            help = "Use Arc<T>, Arc<RwLock<T>>, or Arc<Mutex<T>>."
        );
    }

    (
        clean_inputs,
        resolve_tokens,
        call_args,
        param_entries,
        watcher_select_arms,
    )
}

fn parse_service_attr(attr_str: &str, key: &str) -> Option<String> {
    attr_str.split(',').find_map(|part| {
        let part = part.trim();
        if part.starts_with(key)
            && let Some((_, val)) = part.split_once('=')
        {
            Some(val.trim().to_string())
        } else {
            None
        }
    })
}

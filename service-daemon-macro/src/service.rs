//! `#[service]` macro implementation.

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use crate::common::has_allow_sync;

/// Implementation of the `#[service]` attribute macro.
pub fn service_impl(_attr: TokenStream, item: TokenStream) -> TokenStream {
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
    let mut mutability_marks = Vec::new();

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
                        crate::common::WrapperKind::Arc => format!("Arc<{}>", type_str),
                        crate::common::WrapperKind::ArcRwLock => {
                            format!("Arc<RwLock<{}>>", type_str)
                        }
                        crate::common::WrapperKind::ArcMutex => format!("Arc<Mutex<{}>>", type_str),
                    };

                    match wrapper {
                        crate::common::WrapperKind::Arc => {
                            resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                            });
                            clean_inputs.push(
                                syn::parse2(quote! { #arg_name: std::sync::Arc<#inner_type> })
                                    .unwrap(),
                            );
                        }
                        crate::common::WrapperKind::ArcRwLock => {
                            resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve_rwlock().await;
                            });
                            clean_inputs.push(syn::parse2(quote! { #arg_name: std::sync::Arc<service_daemon::utils::managed_state::RwLock<#inner_type>> }).unwrap());
                            // Emit mutability mark for promotion
                            let mark_name = format_ident!(
                                "__MUT_MARK_{}_{}",
                                fn_name.to_string().to_uppercase(),
                                arg_name.to_string().to_uppercase()
                            );
                            mutability_marks.push(quote! {
                                #[service_daemon::linkme::distributed_slice(service_daemon::models::mutability::MUTABILITY_REGISTRY)]
                                #[linkme(crate = service_daemon::linkme)]
                                static #mark_name: service_daemon::models::mutability::MutabilityMark = service_daemon::models::mutability::MutabilityMark {
                                    key: #type_str
                                };
                            });
                        }
                        crate::common::WrapperKind::ArcMutex => {
                            resolve_tokens.push(quote! {
                                let #arg_name = <#inner_type as service_daemon::Provided>::resolve_mutex().await;
                            });
                            clean_inputs.push(syn::parse2(quote! { #arg_name: std::sync::Arc<service_daemon::utils::managed_state::Mutex<#inner_type>> }).unwrap());
                            // Emit mutability mark for promotion
                            let mark_name = format_ident!(
                                "__MUT_MARK_{}_{}",
                                fn_name.to_string().to_uppercase(),
                                arg_name.to_string().to_uppercase()
                            );
                            mutability_marks.push(quote! {
                                #[service_daemon::linkme::distributed_slice(service_daemon::models::mutability::MUTABILITY_REGISTRY)]
                                #[linkme(crate = service_daemon::linkme)]
                                static #mark_name: service_daemon::models::mutability::MutabilityMark = service_daemon::models::mutability::MutabilityMark {
                                    key: #type_str
                                };
                            });
                        }
                    }

                    call_args.push(quote! { #arg_name });
                    let key_str = format!("{}_{}", arg_name_str, arg_type_wrapper_str);
                    param_entries.push(quote! {
                        service_daemon::ServiceParam { name: #arg_name_str, type_name: #type_str, key: #key_str }
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

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let wrapper_name = format_ident!("{}_wrapper", fn_name);
    let entry_name = format_ident!("__SERVICE_ENTRY_{}", fn_name.to_string().to_uppercase());

    // Get module name from function path (simplified - uses "services" as default)
    let module_name = "services";

    let is_async = input.sig.asyncness.is_some();
    let allow_sync_present = has_allow_sync(attrs);
    let call_expr = if is_async {
        quote! { #fn_name(#(#call_args),*).await }
    } else if allow_sync_present {
        // User explicitly allowed sync, no warning
        quote! { #fn_name(#(#call_args),*) }
    } else {
        quote! {
            {
                tracing::warn!("Service '{}' is synchronous. Consider switching to 'async fn' to avoid blocking the executor.", #fn_name_str);
                #fn_name(#(#call_args),*)
            }
        }
    };

    let expanded = quote! {
        #(#attrs)*
        #vis #clean_sig {
            // "Macro Illusion": Redirect RwLock/Mutex to our tracked versions
            #[allow(unused_imports)]
            use service_daemon::utils::managed_state::{RwLock, Mutex};
            #body
        }

        /// Auto-generated wrapper for the service - resolves dependencies via Type-Based DI
        pub fn #wrapper_name(token: service_daemon::tokio_util::sync::CancellationToken) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #(#resolve_tokens)*
                #call_expr
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

        #(#mutability_marks)*
    };

    TokenStream::from(expanded)
}

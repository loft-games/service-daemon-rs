//! `#[service]` macro implementation.

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, Pat, Type, parse_macro_input};

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

    for arg in &sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let arg_name = &pat_ident.ident;
                let arg_type = &pat_type.ty;
                let arg_name_str = arg_name.to_string();
                let arg_type_str = quote!(#arg_type).to_string().replace(" ", "");

                // Check if the type is Arc<T>
                if let Type::Path(type_path) = &**arg_type {
                    if let Some(segment) = type_path.path.segments.last() {
                        if segment.ident == "Arc" {
                            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                                if let Some(syn::GenericArgument::Type(inner_type)) =
                                    args.args.first()
                                {
                                    // Type-Based DI: use T::resolve().await for async resolution
                                    resolve_tokens.push(quote! {
                                        let #arg_name = <#inner_type as service_daemon::Provided>::resolve().await;
                                    });
                                    call_args.push(quote! { #arg_name });

                                    // Build param entry for registry with pre-computed key
                                    let key_str = format!("{}_{}", arg_name_str, arg_type_str);
                                    param_entries.push(quote! {
                                        service_daemon::ServiceParam { name: #arg_name_str, type_name: #arg_type_str, key: #key_str }
                                    });
                                    continue;
                                }
                            }
                        }
                    }
                }

                // Non-Arc types are not supported for DI
                abort!(
                    arg_type,
                    "Service parameters must be Arc<T> where T implements Provided";
                    help = "Wrap your type in Arc<T>, e.g., `Arc<MyType>` instead of `MyType`";
                    note = "The DI system requires Arc<T> to manage shared ownership of providers"
                );
            }
        }
    }

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
        #vis #sig {
            #body
        }

        /// Auto-generated wrapper for the service - resolves dependencies via Type-Based DI
        pub fn #wrapper_name() -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
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
    };

    TokenStream::from(expanded)
}

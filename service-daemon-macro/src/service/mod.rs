//! `#[service]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `codegen`: Code generation for watchers and call expressions.

mod codegen;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parse;
use syn::{ItemFn, Token, parse_macro_input};

use crate::common::{ExtractedParams, TagsList, extract_sync_handler_flag};
use codegen::{generate_call_expr, generate_watcher};

// ─────────────────────────────────────────────────────────────────────────────
// Structured attribute parser (replaces the old string-based parse_service_attr)
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed result of `#[service(...)]` attributes.
///
/// Supports the following syntax:
/// ```ignore
/// #[service]                                        // all defaults
/// #[service(priority = 80)]                         // priority only
/// #[service(tags = ["infra", "core"])]              // tags only
/// #[service(priority = 80, tags = ["infra"])]       // both
/// ```
struct ServiceAttr {
    priority: proc_macro2::TokenStream,
    tags: proc_macro2::TokenStream,
}

impl Parse for ServiceAttr {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut priority: proc_macro2::TokenStream = quote!(50);
        let mut tags: proc_macro2::TokenStream = quote!(&[]);

        // Parse comma-separated key=value pairs
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "priority" => {
                    let value: syn::Expr = input.parse()?;
                    priority = quote!(#value);
                }
                "tags" => {
                    let tag_list: TagsList = input.parse()?;
                    tags = tag_list.to_tokens();
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "Unknown service attribute '{}'. Supported: priority, tags",
                            other
                        ),
                    ));
                }
            }

            // Consume optional trailing comma
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(ServiceAttr { priority, tags })
    }
}

/// Implementation of the `#[service]` attribute macro.
pub fn service_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse attributes using syn-based structured parsing
    let args = parse_macro_input!(attr as ServiceAttr);
    let priority_tokens = args.priority;
    let tags_tokens = args.tags;

    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;

    // Detect #[allow(sync_handler)] and strip it from the attribute list
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&input.attrs);

    // Extract parameters and categorize them
    let ExtractedParams {
        clean_inputs,
        resolve_tokens,
        call_args,
        param_entries,
        watcher_arms,
    } = crate::common::extract_params(sig, false);

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let wrapper_name = format_ident!("{}_wrapper", fn_name);
    let entry_name = format_ident!("__SERVICE_ENTRY_{}", fn_name.to_string().to_uppercase());

    let is_async = input.sig.asyncness.is_some();
    let call_expr = generate_call_expr(
        fn_name,
        &fn_name_str,
        &call_args,
        is_async,
        allow_sync_present,
    );

    let (watcher_fn, watcher_ptr) = generate_watcher(fn_name, &watcher_arms);

    let expanded = quote! {
        #(#cleaned_attrs)*
        #vis #clean_sig {
            // "Macro Illusion": Redirect RwLock/Mutex to our tracked versions
            #[allow(unused_imports)]
            use service_daemon::core::managed_state::{RwLock, Mutex};
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
            tags: #tags_tokens,
        };

    };

    TokenStream::from(expanded)
}

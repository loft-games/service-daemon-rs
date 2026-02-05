//! `#[service]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `codegen`: Code generation for watchers and call expressions.

mod codegen;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use crate::common::{ExtractedParams, has_allow_sync};
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
    let allow_sync_present = has_allow_sync(attrs);
    let call_expr = generate_call_expr(
        fn_name,
        &fn_name_str,
        &call_args,
        is_async,
        allow_sync_present,
    );

    let (watcher_fn, watcher_ptr) = generate_watcher(fn_name, &watcher_arms);

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

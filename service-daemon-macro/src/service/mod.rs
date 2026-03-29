//! `#[service]` macro implementation.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use crate::common::{ExtractedParams, extract_sync_handler_flag, scope_inner_visibility};
use crate::common::{generate_call_expr, generate_watcher};

mod parser;

use parser::ServiceAttr;

/// Implementation of the `#[service]` attribute macro.
pub fn service_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse attributes using syn-based structured parsing
    let args = parse_macro_input!(attr as ServiceAttr);
    let priority_tokens = args.priority;
    let scheduling_tokens = args.scheduling;
    let tags_tokens = args.tags;

    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;

    // Detect #[allow(sync_handler)] and strip it from the attribute list
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&input.attrs);

    // `extract_params(..., false)` uses the shared parser in service mode.
    // Bare or `#[payload]` parameters still flow through the shared payload
    // lane internally, but that lane is only used to reject invalid service
    // signatures. `#[service]` parameters must be Arc-based dependencies.
    let ExtractedParams {
        clean_inputs,
        resolve_tokens,
        call_args,
        param_entries,
        watcher_arms,
        ..
    } = crate::common::extract_params(sig, false);

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let wrapper_name = format_ident!("{}_wrapper", fn_name);
    let entry_name = format_ident!("__SERVICE_ENTRY_{}", fn_name.to_string().to_uppercase());
    let scope_mod = format_ident!(
        "__SERVICE_USER_SCOPE_{}",
        fn_name.to_string().to_uppercase()
    );

    let is_async = input.sig.asyncness.is_some();
    let call_expr = generate_call_expr(
        fn_name,
        &fn_name_str,
        &call_args,
        is_async,
        allow_sync_present,
        "Service",
    );

    let (watcher_fn, watcher_ptr) = generate_watcher(fn_name, &watcher_arms);

    let inner_vis = scope_inner_visibility(vis);
    let user_scope = crate::common::generate_user_scope_mod(
        &scope_mod,
        &inner_vis,
        &clean_sig,
        &cleaned_attrs,
        body,
    );

    let wrapper_fn = crate::common::generate_wrapper_fn(
        &wrapper_name,
        &quote! {
            #(#resolve_tokens)*
            #call_expr
        },
    );

    let registry_entry =
        crate::common::generate_static_registry_entry(crate::common::RegistryEntryInput {
            entry_name: &entry_name,
            fn_name_str: &fn_name_str,
            param_entries: &param_entries,
            wrapper_name: &wrapper_name,
            watcher_ptr: &watcher_ptr,
            priority: &priority_tokens,
            scheduling: &scheduling_tokens,
            tags: &tags_tokens,
        });

    let expanded = quote! {
        #user_scope

        #vis use #scope_mod::#fn_name;

        #wrapper_fn

        #watcher_fn

        #registry_entry
    };

    TokenStream::from(expanded)
}

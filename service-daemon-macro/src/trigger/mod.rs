//! `#[trigger]` macro implementation.
//!
//! This module uses the shared codegen helpers from `common.rs` for
//! `generate_call_expr` and `generate_watcher`. Only `generate_event_loop_call`
//! remains trigger-specific.

mod codegen;
mod parser;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use crate::common::{ExtractedParams, extract_sync_handler_flag, scope_inner_visibility};
use crate::common::{generate_call_expr, generate_watcher};
use codegen::generate_event_loop_call;
use parser::TriggerArgs;

/// Implementation of the `#[trigger]` attribute macro.
///
/// The macro resolves the host type via the Rust type system rather than
/// keyword matching. Any type implementing `TriggerHost<Target>` can be
/// used as the first argument.
pub fn trigger_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;

    // Detect #[allow(sync_handler)] and strip it from the attribute list
    let (allow_sync_present, cleaned_attrs) = extract_sync_handler_flag(&input.attrs);

    // Parse attributes using syn-based structured parsing.
    // The host path is now a real Rust path - no keyword validation needed.
    let args = parse_macro_input!(attr as TriggerArgs);
    let host_path = &args.host_path;
    let target_type = args.target;
    let is_watch_host = args.is_watch_host;
    let priority_tokens = args.priority;
    let tags_tokens = args.tags;

    // `extract_params(..., true)` uses the shared parser in trigger mode.
    // Here the shared payload lane is the real trigger payload semantics:
    // exactly one bare or `#[payload]` parameter may be accepted, while
    // Arc-based parameters are treated as framework-managed dependencies.
    let ExtractedParams {
        clean_inputs,
        resolve_tokens,
        call_args,
        param_entries,
        mut watcher_arms,
        di_idents,
    } = crate::common::extract_params(sig, true);

    let is_async = input.sig.asyncness.is_some();
    let call_expr = generate_call_expr(
        fn_name,
        &fn_name_str,
        &call_args,
        is_async,
        allow_sync_present,
        "Trigger",
    );

    // Triggers always watch their target for configuration changes,
    // in addition to any DI dependency watchers from extract_params.
    if is_watch_host {
        watcher_arms.push(quote! {
            _ = <#target_type as service_daemon::WatchableProvided>::changed() => {}
        });
    }
    let (watcher_fn, watcher_ptr) = generate_watcher(fn_name, &watcher_arms);

    let event_loop_call = generate_event_loop_call(
        host_path,
        &fn_name_str,
        &target_type,
        &resolve_tokens,
        &di_idents,
        &call_expr,
    );

    let wrapper_name = format_ident!("{}_trigger_wrapper", fn_name);
    let entry_name = format_ident!("__TRIGGER_ENTRY_{}", fn_name.to_string().to_uppercase());

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let scope_mod = format_ident!(
        "__TRIGGER_USER_SCOPE_{}",
        fn_name.to_string().to_uppercase()
    );

    let inner_vis = scope_inner_visibility(vis);
    let user_scope = crate::common::generate_user_scope_mod(
        &scope_mod,
        &inner_vis,
        &clean_sig,
        &cleaned_attrs,
        body,
    );

    let wrapper_fn = crate::common::generate_wrapper_fn(&wrapper_name, &event_loop_call);

    let registry_entry = crate::common::generate_static_registry_entry(
        &entry_name,
        &fn_name_str,
        &param_entries,
        &wrapper_name,
        &watcher_ptr,
        &priority_tokens,
        &tags_tokens,
    );

    let expanded = quote! {
        #user_scope

        #vis use #scope_mod::#fn_name;

        #wrapper_fn

        #watcher_fn

        #registry_entry
    };

    TokenStream::from(expanded)
}

//! `#[trigger]` macro implementation.
//!
//! This module is split into submodules for better organization:
//! - `parser`: Attribute parsing and validation.
//! - `codegen`: Code generation for event loops and watchers.

mod codegen;
mod parser;

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use crate::common::{ExtractedParams, has_allow_sync};
use codegen::{generate_call_expr, generate_event_loop_call, generate_watcher};
use parser::{TriggerArgs, VALID_VARIANTS, normalize_template};

/// Implementation of the `#[trigger]` attribute macro.
pub fn trigger_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let vis = &input.vis;
    let attrs = &input.attrs;
    let sig = &input.sig;
    let body = &input.block;

    // Parse attributes using syn-based structured parsing
    let args = parse_macro_input!(attr as TriggerArgs);
    let template = args.template;
    let target_type = args.target;
    let priority_tokens = args.priority;
    let tags_tokens = args.tags;

    // Strict compile-time validation of the template variant name
    if !VALID_VARIANTS.contains(&template.as_str()) {
        abort!(
            args.template_ident,
            format!(
                "Invalid trigger template '{}'. Valid options are: {}. \n\
                 Hint: Use variants like Watch(...), Cron(...), Queue(...), etc.",
                template,
                VALID_VARIANTS.join(", ")
            )
        );
    }

    // Extract parameters and categorize them
    let ExtractedParams {
        clean_inputs,
        resolve_tokens,
        call_args,
        param_entries,
        watcher_arms,
    } = crate::common::extract_params(sig, true);

    // Compute normalized template name and enum variant
    let (normalized_template, _template_variant) = normalize_template(&template);

    let is_async = input.sig.asyncness.is_some();
    let allow_sync_present = has_allow_sync(attrs);
    let call_expr = generate_call_expr(
        fn_name,
        &fn_name_str,
        &call_args,
        is_async,
        allow_sync_present,
    );

    let (watcher_fn, watcher_ptr) =
        generate_watcher(fn_name, normalized_template, &watcher_arms, &target_type);

    let event_loop_call = generate_event_loop_call(
        normalized_template,
        &fn_name_str,
        &target_type,
        &resolve_tokens,
        &call_expr,
        &template,
    );

    let wrapper_name = format_ident!("{}_trigger_wrapper", fn_name);
    let entry_name = format_ident!("__TRIGGER_ENTRY_{}", fn_name.to_string().to_uppercase());

    let mut clean_sig = sig.clone();
    clean_sig.inputs = clean_inputs;

    let expanded = quote! {
        #(#attrs)*
        #vis #clean_sig {
            // "Macro Illusion": Redirect RwLock/Mutex to our tracked versions
            #[allow(unused_imports)]
            use service_daemon::core::managed_state::{RwLock, Mutex};
            #body
        }

        /// Auto-generated trigger wrapper - acts as an event-loop "Call Host"
        /// This is registered as a Service, so it benefits from ServiceDaemon's lifecycle management.
        pub fn #wrapper_name(token: service_daemon::tokio_util::sync::CancellationToken) -> service_daemon::futures::future::BoxFuture<'static, anyhow::Result<()>> {
            Box::pin(async move {
                #event_loop_call
            })
        }

        #watcher_fn

        /// Auto-generated static registry entry - triggers are specialized services
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

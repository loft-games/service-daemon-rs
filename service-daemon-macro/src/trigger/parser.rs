//! Attribute parsing for the `#[trigger]` macro.
//!
//! Supports the modern syntax:
//!   `#[trigger(Watch(MetricsData), priority = 80)]`
//!
//! The first argument is always a template call in the form `Template(Target)`.
//! `Template` is any type path that implements `TriggerHost<Target>` — no
//! keyword validation is performed here; the compiler will catch invalid types.
//! Optional named arguments like `priority = N` follow after a comma.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token, parenthesized};

use crate::common::TagsList;

/// Parsed result of `#[trigger(...)]` attributes.
///
/// Captures the host type path, the target type, and optional
/// named parameters like `priority`.
pub struct TriggerArgs {
    /// The host type as a full path (e.g., `Notify`, `TT::Queue`, `crate::MyHost`).
    /// This is passed directly to the generated code as
    /// `<#host_path as TriggerHost<#target>>::run_as_service(...)`.
    pub host_path: syn::Path,
    /// The target type as a token stream (e.g., `MetricsData`, `crate::providers::JobQueue`).
    pub target: TokenStream,
    /// Optional priority value (defaults to 50 if not specified).
    pub priority: TokenStream,
    /// Optional tags for registry filtering (defaults to empty).
    pub tags: TokenStream,
}

/// Parses the token stream inside `#[trigger(...)]`.
///
/// Expected grammar:
///   `HostPath(TargetType)` [, `priority` = EXPR]*
///
/// Where `HostPath` is any valid Rust type path (e.g., `Watch`, `TT::Queue`,
/// `service_daemon::TT::Cron`) and `TargetType` is any valid Rust type path.
///
/// No compile-time validation of the host path is performed — if the path
/// does not refer to a type implementing `TriggerHost<Target>`, the Rust
/// compiler will emit a clear error at the call site.
impl Parse for TriggerArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Step 1: Parse the host as a Path (e.g., Watch, TT::Watch, service_daemon::TT::Watch)
        //         Using syn::Path allows LSPs like rust-analyzer to "see" a Rust path
        //         and provide completions based on what's in scope.
        let host_path: syn::Path = input.parse()?;

        // Step 2: Parse the parenthesized target type.
        //         e.g., `(MetricsData)` or `(crate::providers::JobQueue)`
        let content;
        parenthesized!(content in input);
        let target: TokenStream = content.parse()?;

        // Step 3: Parse optional trailing named arguments.
        let mut priority: TokenStream = quote!(50);
        let mut tags: TokenStream = quote!(&[]);
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            // Allow trailing comma with nothing after it
            if input.is_empty() {
                break;
            }

            let key: Ident = input.parse()?;
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
                            "Unknown trigger attribute '{}'. Supported: priority, tags",
                            other
                        ),
                    ));
                }
            }
        }

        Ok(TriggerArgs {
            host_path,
            target,
            priority,
            tags,
        })
    }
}

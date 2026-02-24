//! Attribute parsing for the `#[trigger]` macro.
//!
//! Supports the modern syntax:
//!   `#[trigger(Watch(MetricsData), priority = 80)]`
//!
//! The first argument is always a template call in the form `Template(Target)`.
//! Optional named arguments like `priority = N` follow after a comma.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Ident, Token, parenthesized};

/// The list of valid trigger template variant names.
///
/// These correspond to variants of `TriggerTemplate` in the runtime crate.
/// Multiple aliases may map to the same internal template via `normalize_template`.
pub const VALID_VARIANTS: &[&str] = &[
    "Notify",
    "Event",
    "Signal",
    "Custom",
    "Queue",
    "BQueue",
    "BroadcastQueue",
    "LBQueue",
    "LoadBalancingQueue",
    "Cron",
    "Watch",
    "State",
];

/// Parsed result of `#[trigger(...)]` attributes.
///
/// Captures the template variant name, the target type, and optional
/// named parameters like `priority`.
pub struct TriggerArgs {
    /// The template variant name (e.g., "Watch", "Cron", "Queue").
    pub template: String,
    /// The span of the template identifier, for error reporting.
    pub template_ident: Ident,
    /// The target type as a token stream (e.g., `MetricsData`, `crate::providers::JobQueue`).
    pub target: TokenStream,
    /// Optional priority value (defaults to 50 if not specified).
    pub priority: TokenStream,
}

/// Parses the token stream inside `#[trigger(...)]`.
///
/// Expected grammar:
///   `Template(TargetType)` [, `priority` = EXPR]*
///
/// Where `Template` is a valid trigger template variant (see `VALID_VARIANTS`)
/// and `TargetType` is any valid Rust type path.
impl Parse for TriggerArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Step 1: Parse the template as a Path (e.g., Watch, TT::Watch, service_daemon::TT::Watch)
        //         Using syn::Path allows LSPs like rust-analyzer to "see" a Rust path
        //         and provide completions based on what's in scope.
        let path: syn::Path = input.parse()?;
        let template_ident = path
            .segments
            .last()
            .ok_or_else(|| syn::Error::new(path.span(), "Empty template path"))?
            .ident
            .clone();
        let template = template_ident.to_string();

        // Step 2: Parse the parenthesized target type.
        //         e.g., `(MetricsData)` or `(crate::providers::JobQueue)`
        let content;
        parenthesized!(content in input);
        let target: TokenStream = content.parse()?;

        // Step 3: Parse optional trailing named arguments.
        let mut priority: TokenStream = quote!(50);
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
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("Unknown trigger attribute '{}'. Supported: priority", other),
                    ));
                }
            }
        }

        Ok(TriggerArgs {
            template,
            template_ident,
            target,
            priority,
        })
    }
}

/// Normalizes the template variant name to a canonical internal form.
///
/// This allows multiple aliases (e.g., "Event", "Signal", "Custom") to map to
/// the same internal template ("notify"), simplifying code generation.
///
/// Returns `(normalized_template, template_variant)` where:
/// - `normalized_template` is a lowercase key used in `match` statements for code generation.
/// - `template_variant` is the canonical enum variant name (e.g., "Notify").
pub fn normalize_template(template: &str) -> (&'static str, &'static str) {
    match template {
        "Notify" | "Event" | "Signal" | "Custom" => ("notify", "Notify"),
        "Queue" | "BQueue" | "BroadcastQueue" => ("queue", "Queue"),
        "LBQueue" | "LoadBalancingQueue" => ("lb_queue", "LBQueue"),
        "Cron" => ("cron", "Cron"),
        "Watch" | "State" => ("watch", "Watch"),
        _ => ("unknown", "Unknown"),
    }
}

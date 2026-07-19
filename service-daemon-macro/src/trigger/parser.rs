//! Attribute parsing for the `#[trigger]` macro.
//!
//! Supports the modern syntax:
//!   `#[trigger(Watch(MetricsData), priority = 80, scheduling = HighPriority)]`
//!
//! The first argument is always a template call in the form `Template(Target)`.
//! `Template` is any type path that implements `TriggerHost<Target>` - no
//! keyword validation is performed here; the compiler will catch invalid types.
//! Optional named arguments like `priority = N`, `scheduling = HighPriority`, and `tags = [...]` follow after a comma.

use proc_macro2::TokenStream;
use syn::parenthesized;
use syn::parse::{Parse, ParseStream};

use crate::common::{CommonEntryAttrs, parse_optional_named_tail};

/// Parsed result of `#[trigger(...)]` attributes.
///
/// Captures the host type path, the target type, and optional
/// named parameters like `priority`.
pub struct TriggerArgs {
    /// The host type as a full path (e.g., `Notify`, `TT::Queue`, `crate::MyHost`).
    /// This is passed directly to the generated code as
    /// `<#host_path as TriggerHost<#target>>::run_as_service(...)`.
    pub host_path: syn::Path,
    /// Whether the host is a `Watch` trigger and therefore requires
    /// `WatchableProvided` on the target type.
    pub is_watch_host: bool,
    /// The target type as a token stream (e.g., `MetricsData`, `crate::providers::JobQueue`).
    pub target: TokenStream,
    /// Optional priority value (defaults to 50 if not specified).
    pub priority: TokenStream,
    /// Optional scheduling policy (defaults to Standard).
    pub scheduling: TokenStream,
    /// Optional tags for registry filtering (defaults to empty).
    pub tags: TokenStream,
}

/// Parses the token stream inside `#[trigger(...)]`.
///
/// Expected grammar:
///   `HostPath(TargetType)` [, `priority` = EXPR | `scheduling` = IDENT | `tags` = [...]]*
///
/// Where `HostPath` is any valid Rust type path (e.g., `Watch`, `TT::Queue`,
/// `service_daemon::TT::Cron`) and `TargetType` is any valid Rust type path.
///
/// No compile-time validation of the host path is performed - if the path
/// does not refer to a type implementing `TriggerHost<Target>`, the Rust
/// compiler will emit a clear error at the call site.
impl Parse for TriggerArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Step 1: Parse the host as a Path (e.g., Watch, TT::Watch, service_daemon::TT::Watch)
        //         Using syn::Path allows LSPs like rust-analyzer to "see" a Rust path
        //         and provide completions based on what's in scope.
        let host_path: syn::Path = input.parse()?;
        let is_watch_host = host_path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Watch");

        // Step 2: Parse the parenthesized target type.
        //         e.g., `(MetricsData)` or `(crate::providers::JobQueue)`
        let content;
        parenthesized!(content in input);
        let target: TokenStream = content.parse()?;

        // Step 3: Parse optional trailing named arguments.
        let mut common = CommonEntryAttrs::default();
        parse_optional_named_tail(input, |meta| common.parse_meta_for("trigger", meta))?;

        Ok(TriggerArgs {
            host_path,
            is_watch_host,
            target,
            priority: common.priority,
            scheduling: common.scheduling,
            tags: common.tags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_str;

    #[test]
    fn test_parse_scheduling_default() {
        let args: TriggerArgs = parse_str("Notify(MySignal)").unwrap();
        assert_eq!(
            args.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Standard"
        );
    }

    #[test]
    fn test_parse_scheduling_standard() {
        let args: TriggerArgs = parse_str("Notify(MySignal), scheduling = Standard").unwrap();
        assert_eq!(
            args.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Standard"
        );
    }

    #[test]
    fn test_parse_scheduling_high_priority() {
        let args: TriggerArgs = parse_str("Notify(MySignal), scheduling = HighPriority").unwrap();
        assert_eq!(
            args.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: HighPriority"
        );
    }

    #[test]
    fn test_parse_scheduling_isolated() {
        let args: TriggerArgs = parse_str("Notify(MySignal), scheduling = Isolated").unwrap();
        assert_eq!(
            args.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Isolated"
        );
    }
}

//! Attribute parsing for the `#[provider]` macro.
//!
//! Supports the following syntax forms:
//!
//! - **Empty**: `#[provider]`
//! - **Template**: `#[provider(Notify)]`, `#[provider(Queue(String))]`,
//!   `#[provider(template = Queue(String))]`,
//!   `#[provider(Queue(ComplexJob), capacity = 500)]`,
//!   `#[provider(Listen("0.0.0.0:8080"))]`,
//!   `#[provider(Listen("0.0.0.0:8080"), env = "LISTEN_ADDR")]`,
//!   `#[provider(UnixListen("/run/myapp/sock"))]`,
//!   `#[provider(UnixConnect("/run/peer/sock"), env = "PEER_SOCK", eager = true)]`,
//!   `#[provider(NamedPipeListen(r"\\.\pipe\myapp"))]`,
//!   `#[provider(NamedPipeConnect(r"\\.\pipe\peer"), env = "PEER_PIPE", eager = true)]`
//! - **Default value**: `#[provider(8080)]`, `#[provider("mysql://localhost")]`,
//!   `#[provider(default = 8080)]`,
//!   `#[provider("mysql://localhost", env = "DB_URL")]`
//!
//! ## Two-phase parsing
//!
//! 1. **Phase 1 (Primary)**: Identify `ProviderHead` (Template, DefaultExpr, or Empty)
//!    and capture any parenthesized positional argument tokens.
//! 2. **Phase 2 (Attributes)**: A single unified loop captures all trailing
//!    named arguments (`env = "..."`, `capacity = N`). These are stored on
//!    `ProviderNamedAttrs` regardless of the head.

use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

/// Known template names that trigger struct-body replacement.
const TEMPLATE_NAMES: &[&str] = &[
    "Notify",
    "Event",
    "Queue",
    "BQueue",
    "BroadcastQueue",
    "Listen",
    "UnixListen",
    "UnixConnect",
    "NamedPipeListen",
    "NamedPipeConnect",
];

/// Returns `true` if the identifier matches a known template name.
fn is_template_name(ident: &Ident) -> bool {
    TEMPLATE_NAMES.iter().any(|&name| ident == name)
}

fn unknown_provider_attr_error(key: &Ident, include_head_keys: bool) -> syn::Error {
    let supported = if include_head_keys {
        "default, template, env, capacity, eager"
    } else {
        "env, capacity, eager"
    };
    syn::Error::new(
        key.span(),
        format!(
            "Unknown provider attribute '{}'. Supported: {}",
            key, supported
        ),
    )
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Parsed result of `#[provider(...)]` attributes.
///
/// The `head` field determines the core category while `named` carries shared
/// named arguments (`env`, `capacity`, `eager`).
#[derive(Debug)]
pub struct ProviderArgs {
    /// Core category: empty, template, or value.
    pub head: ProviderHead,
    /// Named provider attributes parsed after the optional head.
    pub named: ProviderNamedAttrs,
}

/// The primary category of the provider.
#[derive(Debug)]
pub enum ProviderHead {
    /// No attributes: `#[provider]`
    Empty,

    /// Template-based provider: `#[provider(Queue(String))]` or `#[provider(Notify)]`
    ///
    /// The macro will replace the struct body with the template's generated code.
    BuiltinTemplate {
        /// The template identifier (e.g., `Queue`, `Notify`, `Event`, `Listen`).
        name: Ident,
        /// Raw parenthesized argument tokens owned by the selected template.
        arg: Option<TemplateArg>,
    },

    /// Value-based provider: `#[provider(8080)]` or `#[provider("mysql://...")]`
    ///
    /// The macro generates a `Default` impl using this value.
    DefaultExpr {
        /// The default value expression, or `None` for env-only providers.
        default_value: Option<syn::Expr>,
    },
}

/// Named provider attributes shared by default-expression and template heads.
#[derive(Debug, Default)]
pub struct ProviderNamedAttrs {
    /// Optional environment variable override (shared across all kinds).
    pub env: Option<syn::LitStr>,
    /// Optional capacity for queue-like templates.
    pub capacity: Option<usize>,
    /// Whether this provider should be initialized eagerly at daemon startup.
    pub eager: bool,
}

/// Raw template argument tokens inside parentheses.
#[derive(Debug)]
pub struct TemplateArg {
    /// The tokens inside the template parentheses.
    pub tokens: proc_macro2::TokenStream,
}

fn parse_capacity_literal(lit: syn::LitInt) -> syn::Result<usize> {
    let capacity = lit.base10_parse::<usize>()?;
    if capacity == 0 {
        return Err(syn::Error::new(
            lit.span(),
            "provider capacity must be greater than zero",
        ));
    }
    Ok(capacity)
}

fn parse_eager_literal(input: ParseStream, key: &Ident) -> syn::Result<bool> {
    if input.fork().parse::<syn::LitBool>().is_err() {
        return Err(syn::Error::new(
            key.span(),
            "provider attribute `eager` expects a boolean literal: eager = true or eager = false",
        ));
    }
    Ok(input.parse::<syn::LitBool>()?.value)
}

fn set_once<T>(slot: &mut Option<T>, key: &Ident, attr_name: &str, value: T) -> syn::Result<()> {
    if slot.is_some() {
        return Err(syn::Error::new(
            key.span(),
            format!("duplicate provider attribute `{}`", attr_name),
        ));
    }
    *slot = Some(value);
    Ok(())
}

fn set_eager(eager: &mut bool, eager_seen: &mut bool, key: &Ident, value: bool) -> syn::Result<()> {
    if *eager_seen {
        return Err(syn::Error::new(
            key.span(),
            "duplicate provider attribute `eager`",
        ));
    }
    *eager = value;
    *eager_seen = true;
    Ok(())
}

fn parse_template_head(input: ParseStream, name: Ident) -> syn::Result<ProviderHead> {
    // Capture the parenthesized argument without interpreting it.
    // Template-specific parsing belongs to the template codegen
    // branch, not to the central provider head classifier.
    let mut arg = None;
    if input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in input);
        let tokens: proc_macro2::TokenStream = content.parse()?;
        arg = Some(TemplateArg { tokens });
    }

    Ok(ProviderHead::BuiltinTemplate { name, arg })
}

fn parse_explicit_template_head(input: ParseStream) -> syn::Result<ProviderHead> {
    let name: Ident = input.parse()?;
    if !is_template_name(&name) {
        return Err(syn::Error::new(
            name.span(),
            format!(
                "Unknown provider template '{}'\n\n  = help: Supported templates: Notify, Event, Queue, BQueue, BroadcastQueue, Listen, UnixListen, UnixConnect, NamedPipeListen, NamedPipeConnect\n",
                name
            ),
        ));
    }
    parse_template_head(input, name)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parses the token stream inside `#[provider(...)]`.
///
/// Grammar:
///   - Empty
///   - `TemplateIdent` [`(` TokenStream `)`] [`,` NamedArg]*
///   - `default` `=` Expr [`,` NamedArg]*
///   - `template` `=` TemplateIdent [`(` TokenStream `)`] [`,` NamedArg]*
///   - `Ident` `=` Value [`,` NamedArg]*  (env/eager-only shorthand)
///   - Literal [`,` NamedArg]*
///
/// Named args: `env = "..."`, `capacity = N`
impl Parse for ProviderArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Empty attributes: #[provider]
        if input.is_empty() {
            return Ok(ProviderArgs {
                head: ProviderHead::Empty,
                named: ProviderNamedAttrs::default(),
            });
        }

        // == Phase 1: Identify the primary kind ==========================

        let head = if input.peek(Ident) {
            let ident: Ident = input.fork().parse()?;

            if is_template_name(&ident) {
                // Consume the identifier from the real stream
                let name: Ident = input.parse()?;
                parse_template_head(input, name)?
            } else if input.peek2(Token![=]) {
                // Named head or named-arg-only - e.g.,
                // `#[provider(default = 8080)]`, `#[provider(template = Queue(String))]`,
                // or `#[provider(env = "API_KEY")]`.
                // Consume the key=value directly (no comma prefix).
                let key: Ident = input.parse()?;
                input.parse::<Token![=]>()?;

                let mut named = ProviderNamedAttrs::default();
                let mut capacity_span = None;
                let mut eager_seen = false;
                let head = match key.to_string().as_str() {
                    "default" => ProviderHead::DefaultExpr {
                        default_value: Some(input.parse::<syn::Expr>()?),
                    },
                    "template" => parse_explicit_template_head(input)?,
                    "env" => {
                        set_once(&mut named.env, &key, "env", input.parse::<syn::LitStr>()?)?;
                        ProviderHead::DefaultExpr {
                            default_value: None,
                        }
                    }
                    "capacity" => {
                        let lit: syn::LitInt = input.parse()?;
                        set_once(
                            &mut named.capacity,
                            &key,
                            "capacity",
                            parse_capacity_literal(lit)?,
                        )?;
                        capacity_span = Some(key.span());
                        ProviderHead::DefaultExpr {
                            default_value: None,
                        }
                    }
                    "eager" => {
                        let value = parse_eager_literal(input, &key)?;
                        set_eager(&mut named.eager, &mut eager_seen, &key, value)?;
                        ProviderHead::DefaultExpr {
                            default_value: None,
                        }
                    }
                    _ => {
                        return Err(unknown_provider_attr_error(&key, true));
                    }
                };

                // Continue with phase 2 for any remaining `, key = value` pairs
                return Self::parse_trailing_attrs(input, head, named, capacity_span, eager_seen);
            } else {
                // Not a template name - treat as an expression
                // (e.g., a constant identifier used as a default value)
                let default_value: syn::Expr = input.parse()?;
                ProviderHead::DefaultExpr {
                    default_value: Some(default_value),
                }
            }
        } else {
            // Literal or expression - default value
            let default_value: syn::Expr = input.parse()?;
            ProviderHead::DefaultExpr {
                default_value: Some(default_value),
            }
        };

        // == Phase 2: Core mapping logic =================================
        Self::parse_trailing_attrs(input, head, ProviderNamedAttrs::default(), None, false)
    }
}

impl ProviderArgs {
    /// Shared helper that consumes `, key = value` pairs until the stream
    /// is exhausted, then assembles the final `ProviderArgs`.
    ///
    /// `env` and `capacity` carry values already parsed before
    /// entering the loop (used by the env-only branch).
    fn parse_trailing_attrs(
        input: ParseStream,
        head: ProviderHead,
        mut named: ProviderNamedAttrs,
        mut capacity_span: Option<proc_macro2::Span>,
        mut eager_seen: bool,
    ) -> syn::Result<Self> {
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }

            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "env" => {
                    set_once(&mut named.env, &key, "env", input.parse::<syn::LitStr>()?)?;
                }
                "capacity" => {
                    let lit: syn::LitInt = input.parse()?;
                    set_once(
                        &mut named.capacity,
                        &key,
                        "capacity",
                        parse_capacity_literal(lit)?,
                    )?;
                    capacity_span = Some(key.span());
                }
                "eager" => {
                    let value = parse_eager_literal(input, &key)?;
                    set_eager(&mut named.eager, &mut eager_seen, &key, value)?;
                }
                _ => {
                    return Err(unknown_provider_attr_error(&key, false));
                }
            }
        }

        if named.capacity.is_some() && matches!(head, ProviderHead::DefaultExpr { .. }) {
            return Err(syn::Error::new(
                capacity_span.unwrap_or_else(proc_macro2::Span::call_site),
                "provider attribute `capacity` is only supported on Queue providers",
            ));
        }

        Ok(ProviderArgs { head, named })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    /// Helper: parse a token stream into ProviderArgs.
    fn parse_args(tokens: proc_macro2::TokenStream) -> syn::Result<ProviderArgs> {
        syn::parse2::<ProviderArgs>(tokens)
    }

    #[test]
    fn empty_input_yields_empty_variant() {
        let args = parse_args(quote! {}).unwrap();
        assert!(matches!(args.head, ProviderHead::Empty));
        assert!(args.named.env.is_none());
        assert!(args.named.capacity.is_none());
        assert!(!args.named.eager);
    }

    // -- Template branch ----------------------------------------------------------

    #[test]
    fn notify_template_without_inner_type() {
        let args = parse_args(quote! { Notify }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "Notify");
                assert!(arg.is_none());
            }
            _ => panic!("Expected Template variant"),
        }
        assert!(args.named.env.is_none());
        assert!(args.named.capacity.is_none());
        assert!(!args.named.eager);
    }

    #[test]
    fn event_template_is_recognized() {
        let args = parse_args(quote! { Event }).unwrap();
        assert!(
            matches!(&args.head, ProviderHead::BuiltinTemplate { name, .. } if name == "Event")
        );
    }

    #[test]
    fn queue_template_with_inner_type() {
        let args = parse_args(quote! { Queue(String) }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "Queue");
                assert!(
                    matches!(arg, Some(TemplateArg { .. })),
                    "arg should be captured for Queue(String)"
                );
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn queue_template_with_capacity() {
        let args = parse_args(quote! { Queue(String), capacity = 500 }).unwrap();
        assert!(matches!(&args.head, ProviderHead::BuiltinTemplate { .. }));
        assert_eq!(args.named.capacity, Some(500));
    }

    #[test]
    fn explicit_template_key_with_capacity() {
        let args = parse_args(quote! { template = Queue(String), capacity = 500 }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "Queue");
                assert!(matches!(arg, Some(TemplateArg { .. })));
            }
            _ => panic!("Expected explicit template head"),
        }
        assert_eq!(args.named.capacity, Some(500));
    }

    #[test]
    fn capacity_zero_is_rejected() {
        let err = parse_args(quote! { Queue(String), capacity = 0 }).unwrap_err();
        assert!(
            err.to_string()
                .contains("capacity must be greater than zero")
        );
    }

    #[test]
    fn bqueue_alias_is_recognized() {
        let args = parse_args(quote! { BQueue(i32) }).unwrap();
        assert!(
            matches!(&args.head, ProviderHead::BuiltinTemplate { name, .. } if name == "BQueue")
        );
    }

    // -- Value branch -------------------------------------------------------------

    #[test]
    fn integer_literal_default() {
        let args = parse_args(quote! { 8080 }).unwrap();
        match &args.head {
            ProviderHead::DefaultExpr { default_value } => {
                assert!(
                    default_value.is_some(),
                    "default_value should be Some for literal"
                );
            }
            _ => panic!("Expected Value variant"),
        }
        assert!(args.named.env.is_none());
        assert!(!args.named.eager);
    }

    #[test]
    fn string_literal_default() {
        let args = parse_args(quote! { "mysql://localhost" }).unwrap();
        assert!(matches!(&args.head, ProviderHead::DefaultExpr { .. }));
    }

    #[test]
    fn string_default_with_env() {
        let args = parse_args(quote! { "fallback", env = "MY_VAR" }).unwrap();
        assert!(matches!(&args.head, ProviderHead::DefaultExpr { .. }));
        assert_eq!(args.named.env.as_ref().unwrap().value(), "MY_VAR");
    }

    #[test]
    fn explicit_default_key_with_env() {
        let args = parse_args(quote! { default = "fallback", env = "MY_VAR" }).unwrap();
        assert!(matches!(
            &args.head,
            ProviderHead::DefaultExpr {
                default_value: Some(_)
            }
        ));
        assert_eq!(args.named.env.as_ref().unwrap().value(), "MY_VAR");
    }

    // -- Named-arg-only branch ----------------------------------------------------

    #[test]
    fn env_only_without_default() {
        let args = parse_args(quote! { env = "API_KEY" }).unwrap();
        match &args.head {
            ProviderHead::DefaultExpr { default_value } => {
                assert!(
                    default_value.is_none(),
                    "env-only should have None default_value"
                );
            }
            _ => panic!("Expected Value variant with env-only"),
        }
        assert_eq!(args.named.env.as_ref().unwrap().value(), "API_KEY");
        assert!(!args.named.eager);
    }

    #[test]
    fn eager_bool_parses() {
        let args = parse_args(quote! { eager = true }).unwrap();
        assert!(
            matches!(&args.head, ProviderHead::DefaultExpr { default_value } if default_value.is_none())
        );
        assert!(args.named.eager);
    }

    // -- Unknown ident falls through to expression --------------------------------

    #[test]
    fn unknown_ident_treated_as_expression() {
        // A non-template ident (e.g., a constant) should parse as a Value expression
        let args = parse_args(quote! { MY_CONST }).unwrap();
        assert!(matches!(&args.head, ProviderHead::DefaultExpr { .. }));
    }

    // -- Error cases ---------------------------------------------------------------

    #[test]
    fn unknown_template_attribute_is_error() {
        let result = parse_args(quote! { Queue(String), bogus = 42 });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown provider attribute"),
            "Error message should mention unknown provider attribute, got: {}",
            err_msg
        );
    }

    #[test]
    fn explicit_unknown_template_is_error() {
        let result = parse_args(quote! { template = Custom(String) });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown provider template"),
            "Error message should mention unknown provider template, got: {}",
            err_msg
        );
    }

    #[test]
    fn unknown_value_attribute_is_error() {
        let result = parse_args(quote! { 8080, bogus = "x" });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown provider attribute"),
            "Error message should mention unknown provider attribute, got: {}",
            err_msg
        );
    }

    #[test]
    fn duplicate_env_is_error() {
        let err = parse_args(quote! { Notify, env = "A", env = "B" }).unwrap_err();
        assert!(
            err.to_string()
                .contains("duplicate provider attribute `env`")
        );
    }

    #[test]
    fn duplicate_capacity_is_error() {
        let err = parse_args(quote! { Queue(String), capacity = 10, capacity = 20 }).unwrap_err();
        assert!(
            err.to_string()
                .contains("duplicate provider attribute `capacity`")
        );
    }

    #[test]
    fn duplicate_eager_is_error() {
        let err =
            parse_args(quote! { UnixConnect("/sock"), eager = true, eager = false }).unwrap_err();
        assert!(
            err.to_string()
                .contains("duplicate provider attribute `eager`")
        );
    }

    #[test]
    fn malformed_eager_is_error() {
        let err = parse_args(quote! { UnixConnect("/sock"), eager = yes }).unwrap_err();
        assert!(err.to_string().contains("expects a boolean literal"));
    }

    #[test]
    fn value_provider_capacity_is_error() {
        let err = parse_args(quote! { "fallback", capacity = 10 }).unwrap_err();
        assert!(
            err.to_string()
                .contains("provider attribute `capacity` is only supported on Queue providers")
        );
    }

    #[test]
    fn value_provider_capacity_zero_is_rejected() {
        let err = parse_args(quote! { "fallback", capacity = 0 }).unwrap_err();
        assert!(
            err.to_string()
                .contains("provider capacity must be greater than zero")
        );
    }

    // -- Listen template branch ---------------------------------------------------

    #[test]
    fn listen_template_with_addr() {
        let args = parse_args(quote! { Listen("0.0.0.0:8080") }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "Listen");
                match arg {
                    Some(arg) => assert_eq!(arg.tokens.to_string(), "\"0.0.0.0:8080\""),
                    _ => panic!("Expected captured arg"),
                }
            }
            _ => panic!("Expected Template variant"),
        }
        assert!(args.named.env.is_none());
        assert!(!args.named.eager);
    }

    #[test]
    fn listen_template_with_addr_and_env() {
        let args = parse_args(quote! { Listen("0.0.0.0:8080"), env = "LISTEN_ADDR" }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "Listen");
                match arg {
                    Some(arg) => assert_eq!(arg.tokens.to_string(), "\"0.0.0.0:8080\""),
                    _ => panic!("Expected captured arg"),
                }
            }
            _ => panic!("Expected Template variant with env"),
        }
        assert_eq!(args.named.env.as_ref().unwrap().value(), "LISTEN_ADDR");
    }

    #[test]
    fn listen_template_unknown_attr_is_error() {
        let result = parse_args(quote! { Listen("0.0.0.0:8080"), bogus = "x" });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown provider attribute"),
            "Error message should mention unknown provider attribute, got: {}",
            err_msg
        );
    }

    #[test]
    fn listen_template_captures_inner_named_tokens_for_template_validation() {
        let args = parse_args(quote! { Listen("0.0.0.0:8080", env = "LISTEN_ADDR") }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { arg: Some(arg), .. } => {
                assert!(arg.tokens.to_string().contains("env"));
            }
            _ => panic!("Expected captured template arg"),
        }
    }

    // -- Cross-template tests (env on non-Listen templates) -----------------------

    #[test]
    fn notify_template_with_env_parses_ok() {
        // env is a shared attribute, should parse successfully even for Notify
        let args = parse_args(quote! { Notify, env = "SOME_VAR" }).unwrap();
        assert!(
            matches!(&args.head, ProviderHead::BuiltinTemplate { name, .. } if name == "Notify")
        );
        assert_eq!(args.named.env.as_ref().unwrap().value(), "SOME_VAR");
    }

    #[test]
    fn queue_template_with_env_parses_ok() {
        let args = parse_args(quote! { Queue(String), env = "Q_VAR" }).unwrap();
        assert!(matches!(&args.head, ProviderHead::BuiltinTemplate { .. }));
        assert_eq!(args.named.env.as_ref().unwrap().value(), "Q_VAR");
    }

    #[test]
    fn queue_template_with_capacity_and_env() {
        let args = parse_args(quote! { Queue(String), capacity = 200, env = "Q_VAR" }).unwrap();
        assert_eq!(args.named.capacity, Some(200));
        assert_eq!(args.named.env.as_ref().unwrap().value(), "Q_VAR");
    }

    // -- UnixListen / UnixConnect template branches -------------------------------

    #[test]
    fn unix_listen_template_with_path() {
        let args = parse_args(quote! { UnixListen("/run/myapp/sock") }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "UnixListen");
                match arg {
                    Some(arg) => assert_eq!(arg.tokens.to_string(), "\"/run/myapp/sock\""),
                    _ => panic!("Expected captured arg for UnixListen"),
                }
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn unix_connect_template_with_path_env_eager() {
        let args =
            parse_args(quote! { UnixConnect("/run/peer/sock"), env = "PEER_SOCK", eager = true })
                .unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "UnixConnect");
                assert!(matches!(arg, Some(TemplateArg { .. })));
            }
            _ => panic!("Expected Template variant"),
        }
        assert_eq!(args.named.env.as_ref().unwrap().value(), "PEER_SOCK");
        assert!(args.named.eager);
    }

    #[test]
    fn unix_listen_captures_inner_named_tokens_for_template_validation() {
        let args = parse_args(quote! { UnixListen("/sock", env = "VAR") }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { arg: Some(arg), .. } => {
                assert!(arg.tokens.to_string().contains("env"));
            }
            _ => panic!("Expected captured template arg"),
        }
    }

    // -- NamedPipeListen / NamedPipeConnect template branches ---------------------

    #[test]
    fn named_pipe_listen_template_with_name() {
        let args = parse_args(quote! { NamedPipeListen(r"\\.\pipe\myapp") }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "NamedPipeListen");
                match arg {
                    Some(arg) => assert!(arg.tokens.to_string().contains("pipe")),
                    _ => panic!("Expected captured arg for NamedPipeListen"),
                }
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn named_pipe_connect_template_with_name_env_eager() {
        let args = parse_args(
            quote! { NamedPipeConnect(r"\\.\pipe\peer"), env = "PEER_PIPE", eager = true },
        )
        .unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { name, arg } => {
                assert_eq!(name.to_string(), "NamedPipeConnect");
                assert!(matches!(arg, Some(TemplateArg { .. })));
            }
            _ => panic!("Expected Template variant"),
        }
        assert_eq!(args.named.env.as_ref().unwrap().value(), "PEER_PIPE");
        assert!(args.named.eager);
    }

    #[test]
    fn named_pipe_listen_captures_inner_named_tokens_for_template_validation() {
        let args =
            parse_args(quote! { NamedPipeListen(r"\\.\pipe\app", env = "PIPE_NAME") }).unwrap();
        match &args.head {
            ProviderHead::BuiltinTemplate { arg: Some(arg), .. } => {
                assert!(arg.tokens.to_string().contains("env"));
            }
            _ => panic!("Expected captured template arg"),
        }
    }
}

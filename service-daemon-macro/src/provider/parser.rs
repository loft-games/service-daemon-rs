//! Attribute parsing for the `#[provider]` macro.
//!
//! Supports the following syntax forms:
//!
//! - **Empty**: `#[provider]`
//! - **Template**: `#[provider(Notify)]`, `#[provider(Queue(String))]`,
//!   `#[provider(Queue(ComplexJob), capacity = 500)]`,
//!   `#[provider(Listen("0.0.0.0:8080"))]`,
//!   `#[provider(Listen("0.0.0.0:8080"), env = "LISTEN_ADDR")]`
//! - **Default value**: `#[provider(8080)]`, `#[provider("mysql://localhost")]`,
//!   `#[provider("mysql://localhost", env = "DB_URL")]`
//!
//! ## Two-phase parsing
//!
//! 1. **Phase 1 (Primary)**: Identify `ProviderKind` (Template, Value, or Empty)
//!    and parse any parenthesized positional argument.
//! 2. **Phase 2 (Attributes)**: A single unified loop captures all trailing
//!    named arguments (`env = "..."`, `capacity = N`). These are stored on
//!    the outer `ProviderArgs` struct regardless of the kind.

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
];

/// Returns `true` if the identifier matches a known template name.
fn is_template_name(ident: &Ident) -> bool {
    TEMPLATE_NAMES.iter().any(|&name| ident == name)
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Parsed result of `#[provider(...)]` attributes.
///
/// Shared named arguments (`env`, `capacity`) live on the outer struct.
/// The `kind` field determines the core category (template vs value vs empty).
#[derive(Debug)]
pub struct ProviderArgs {
    /// Core category: empty, template, or value.
    pub kind: ProviderKind,
    /// Optional environment variable override (shared across all kinds).
    pub env: Option<syn::LitStr>,
    /// Optional capacity for queue-like templates.
    pub capacity: Option<usize>,
}

/// The primary category of the provider.
#[derive(Debug)]
pub enum ProviderKind {
    /// No attributes: `#[provider]`
    Empty,

    /// Template-based provider: `#[provider(Queue(String))]` or `#[provider(Notify)]`
    ///
    /// The macro will replace the struct body with the template's generated code.
    Template {
        /// The template identifier (e.g., `Queue`, `Notify`, `Event`, `Listen`).
        name: Ident,
        /// Parenthesized argument, polymorphic: Type for Queue, LitStr for Listen.
        arg: Option<TemplateArg>,
    },

    /// Value-based provider: `#[provider(8080)]` or `#[provider("mysql://...")]`
    ///
    /// The macro generates a `Default` impl using this value.
    Value {
        /// The default value expression, or `None` for env-only providers.
        default_value: Option<syn::Expr>,
    },
}

/// Polymorphic template argument inside parentheses.
///
/// Different templates expect different argument types in the same
/// syntactic position:
/// - `Queue(String)` -> `TemplateArg::Type`
/// - `Listen("0.0.0.0:8080")` -> `TemplateArg::Addr`
#[derive(Debug)]
pub enum TemplateArg {
    /// A type argument: `Queue(String)`, `Queue(ComplexJob)`
    Type(Box<syn::Type>),
    /// A string literal address: `Listen("0.0.0.0:8080")`
    Addr(syn::LitStr),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parses the token stream inside `#[provider(...)]`.
///
/// Grammar:
///   - Empty
///   - `TemplateIdent` [`(` Type | LitStr `)`] [`,` NamedArg]*
///   - `Ident` `=` Value [`,` NamedArg]*  (env-only shorthand)
///   - Literal [`,` NamedArg]*
///
/// Named args: `env = "..."`, `capacity = N`
impl Parse for ProviderArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Empty attributes: #[provider]
        if input.is_empty() {
            return Ok(ProviderArgs {
                kind: ProviderKind::Empty,
                env: None,
                capacity: None,
            });
        }

        // ── Phase 1: Identify the primary kind ──────────────────────────

        let kind = if input.peek(Ident) {
            let ident: Ident = input.fork().parse()?;

            if is_template_name(&ident) {
                // Consume the identifier from the real stream
                let name: Ident = input.parse()?;
                let is_listen = name == "Listen";

                // Parse parenthesized argument:
                // - Listen: expects a string literal address
                // - Queue/others: expects a type
                let mut arg = None;
                if input.peek(syn::token::Paren) {
                    let content;
                    syn::parenthesized!(content in input);
                    if is_listen {
                        arg = Some(TemplateArg::Addr(content.parse::<syn::LitStr>()?));
                    } else {
                        arg = Some(TemplateArg::Type(Box::new(content.parse::<syn::Type>()?)));
                    }

                    // Reject unexpected trailing tokens inside parentheses.
                    // Named attributes like `env` must be placed outside:
                    //   #[provider(Listen("addr"), env = "VAR")]
                    if !content.is_empty() {
                        return Err(syn::Error::new(
                            content.span(),
                            "Unexpected tokens inside template parentheses; \
                             named attributes like `env` belong outside: \
                             #[provider(Listen(\"addr\"), env = \"VAR\")]",
                        ));
                    }
                }

                ProviderKind::Template { name, arg }
            } else if input.peek2(Token![=]) {
                // Named-arg-only — e.g., `#[provider(env = "API_KEY")]`
                // Consume the key=value directly (no comma prefix).
                let key: Ident = input.parse()?;
                input.parse::<Token![=]>()?;

                let mut env = None;
                let mut capacity = None;
                match key.to_string().as_str() {
                    "env" => {
                        env = Some(input.parse::<syn::LitStr>()?);
                    }
                    "capacity" => {
                        let lit: syn::LitInt = input.parse()?;
                        capacity = Some(lit.base10_parse::<usize>()?);
                    }
                    other => {
                        return Err(syn::Error::new(
                            key.span(),
                            format!(
                                "Unknown provider attribute '{}'. Supported: env, capacity",
                                other
                            ),
                        ));
                    }
                }

                // Continue with phase 2 for any remaining `, key = value` pairs
                return Self::parse_trailing_attrs(
                    input,
                    ProviderKind::Value {
                        default_value: None,
                    },
                    env,
                    capacity,
                );
            } else {
                // Not a template name — treat as an expression
                // (e.g., a constant identifier used as a default value)
                let default_value: syn::Expr = input.parse()?;
                ProviderKind::Value {
                    default_value: Some(default_value),
                }
            }
        } else {
            // Literal or expression — default value
            let default_value: syn::Expr = input.parse()?;
            ProviderKind::Value {
                default_value: Some(default_value),
            }
        };

        // ── Phase 2: Unified trailing named arguments ───────────────────
        Self::parse_trailing_attrs(input, kind, None, None)
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
        kind: ProviderKind,
        mut env: Option<syn::LitStr>,
        mut capacity: Option<usize>,
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
                    env = Some(input.parse::<syn::LitStr>()?);
                }
                "capacity" => {
                    let lit: syn::LitInt = input.parse()?;
                    capacity = Some(lit.base10_parse::<usize>()?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "Unknown provider attribute '{}'. Supported: env, capacity",
                            other
                        ),
                    ));
                }
            }
        }

        Ok(ProviderArgs {
            kind,
            env,
            capacity,
        })
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
        assert!(matches!(args.kind, ProviderKind::Empty));
        assert!(args.env.is_none());
        assert!(args.capacity.is_none());
    }

    // -- Template branch ----------------------------------------------------------

    #[test]
    fn notify_template_without_inner_type() {
        let args = parse_args(quote! { Notify }).unwrap();
        match &args.kind {
            ProviderKind::Template { name, arg } => {
                assert_eq!(name.to_string(), "Notify");
                assert!(arg.is_none());
            }
            _ => panic!("Expected Template variant"),
        }
        assert!(args.env.is_none());
        assert!(args.capacity.is_none());
    }

    #[test]
    fn event_template_is_recognized() {
        let args = parse_args(quote! { Event }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Template { name, .. } if name == "Event"));
    }

    #[test]
    fn queue_template_with_inner_type() {
        let args = parse_args(quote! { Queue(String) }).unwrap();
        match &args.kind {
            ProviderKind::Template { name, arg } => {
                assert_eq!(name.to_string(), "Queue");
                assert!(
                    matches!(arg, Some(TemplateArg::Type(_))),
                    "arg should be Some(Type) for Queue(String)"
                );
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn queue_template_with_capacity() {
        let args = parse_args(quote! { Queue(String), capacity = 500 }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Template { .. }));
        assert_eq!(args.capacity, Some(500));
    }

    #[test]
    fn bqueue_alias_is_recognized() {
        let args = parse_args(quote! { BQueue(i32) }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Template { name, .. } if name == "BQueue"));
    }

    // -- Value branch -------------------------------------------------------------

    #[test]
    fn integer_literal_default() {
        let args = parse_args(quote! { 8080 }).unwrap();
        match &args.kind {
            ProviderKind::Value { default_value } => {
                assert!(
                    default_value.is_some(),
                    "default_value should be Some for literal"
                );
            }
            _ => panic!("Expected Value variant"),
        }
        assert!(args.env.is_none());
    }

    #[test]
    fn string_literal_default() {
        let args = parse_args(quote! { "mysql://localhost" }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Value { .. }));
    }

    #[test]
    fn string_default_with_env() {
        let args = parse_args(quote! { "fallback", env = "MY_VAR" }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Value { .. }));
        assert_eq!(args.env.as_ref().unwrap().value(), "MY_VAR");
    }

    // -- Named-arg-only branch ----------------------------------------------------

    #[test]
    fn env_only_without_default() {
        let args = parse_args(quote! { env = "API_KEY" }).unwrap();
        match &args.kind {
            ProviderKind::Value { default_value } => {
                assert!(
                    default_value.is_none(),
                    "env-only should have None default_value"
                );
            }
            _ => panic!("Expected Value variant with env-only"),
        }
        assert_eq!(args.env.as_ref().unwrap().value(), "API_KEY");
    }

    // -- Unknown ident falls through to expression --------------------------------

    #[test]
    fn unknown_ident_treated_as_expression() {
        // A non-template ident (e.g., a constant) should parse as a Value expression
        let args = parse_args(quote! { MY_CONST }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Value { .. }));
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

    // -- Listen template branch ---------------------------------------------------

    #[test]
    fn listen_template_with_addr() {
        let args = parse_args(quote! { Listen("0.0.0.0:8080") }).unwrap();
        match &args.kind {
            ProviderKind::Template { name, arg } => {
                assert_eq!(name.to_string(), "Listen");
                match arg {
                    Some(TemplateArg::Addr(lit)) => assert_eq!(lit.value(), "0.0.0.0:8080"),
                    _ => panic!("Expected Addr arg"),
                }
            }
            _ => panic!("Expected Template variant"),
        }
        assert!(args.env.is_none());
    }

    #[test]
    fn listen_template_with_addr_and_env() {
        let args = parse_args(quote! { Listen("0.0.0.0:8080"), env = "LISTEN_ADDR" }).unwrap();
        match &args.kind {
            ProviderKind::Template { name, arg } => {
                assert_eq!(name.to_string(), "Listen");
                match arg {
                    Some(TemplateArg::Addr(lit)) => assert_eq!(lit.value(), "0.0.0.0:8080"),
                    _ => panic!("Expected Addr arg"),
                }
            }
            _ => panic!("Expected Template variant with env"),
        }
        assert_eq!(args.env.as_ref().unwrap().value(), "LISTEN_ADDR");
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
    fn listen_template_env_inside_parens_is_error() {
        // Old syntax `Listen("addr", env = "VAR")` should now be rejected.
        // The correct form is `Listen("addr"), env = "VAR"` (outside parentheses).
        let result = parse_args(quote! { Listen("0.0.0.0:8080", env = "LISTEN_ADDR") });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unexpected tokens inside template parentheses"),
            "Error should mention unexpected tokens inside parentheses, got: {}",
            err_msg,
        );
    }

    // -- Cross-template tests (env on non-Listen templates) -----------------------

    #[test]
    fn notify_template_with_env_parses_ok() {
        // env is a shared attribute, should parse successfully even for Notify
        let args = parse_args(quote! { Notify, env = "SOME_VAR" }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Template { name, .. } if name == "Notify"));
        assert_eq!(args.env.as_ref().unwrap().value(), "SOME_VAR");
    }

    #[test]
    fn queue_template_with_env_parses_ok() {
        let args = parse_args(quote! { Queue(String), env = "Q_VAR" }).unwrap();
        assert!(matches!(&args.kind, ProviderKind::Template { .. }));
        assert_eq!(args.env.as_ref().unwrap().value(), "Q_VAR");
    }

    #[test]
    fn queue_template_with_capacity_and_env() {
        let args = parse_args(quote! { Queue(String), capacity = 200, env = "Q_VAR" }).unwrap();
        assert_eq!(args.capacity, Some(200));
        assert_eq!(args.env.as_ref().unwrap().value(), "Q_VAR");
    }
}

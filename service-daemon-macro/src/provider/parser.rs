//! Attribute parsing for the `#[provider]` macro.
//!
//! Supports the following syntax forms:
//!
//! - **Empty**: `#[provider]`
//! - **Template**: `#[provider(Notify)]`, `#[provider(Queue(String))]`,
//!   `#[provider(Queue(ComplexJob), capacity = 500)]`
//! - **Default value**: `#[provider(8080)]`, `#[provider("mysql://localhost")]`,
//!   `#[provider("mysql://localhost", env = "DB_URL")]`
//!
//! The first token determines the parsing branch:
//! - `Ident` → check for known template name (with optional parenthesized type)
//! - `LitInt` / `LitStr` → default value
//! - Empty → no-arg provider

use syn::parse::{Parse, ParseStream};
use syn::{Ident, Token};

/// Known template names that trigger struct-body replacement.
const TEMPLATE_NAMES: &[&str] = &["Notify", "Event", "Queue", "BQueue", "BroadcastQueue"];

/// Returns `true` if the identifier matches a known template name.
fn is_template_name(ident: &Ident) -> bool {
    TEMPLATE_NAMES.iter().any(|&name| ident == name)
}

/// Parsed result of `#[provider(...)]` attributes.
///
/// The first positional argument determines the variant:
/// - Known template identifier → `Template`
/// - Literal value → `Value`
/// - Nothing → `Empty`
#[derive(Debug)]
pub enum ProviderArgs {
    /// No attributes: `#[provider]`
    Empty,

    /// Template-based provider: `#[provider(Queue(String))]` or `#[provider(Notify)]`
    ///
    /// The macro will replace the struct body with the template's generated code.
    Template {
        /// The template identifier (e.g., `Queue`, `Notify`, `Event`).
        name: Ident,
        /// Optional inner type for parameterized templates (e.g., `String` in `Queue(String)`).
        inner_type: Option<syn::Type>,
        /// Optional capacity for queue templates (e.g., `capacity = 500`).
        capacity: Option<usize>,
    },

    /// Value-based provider: `#[provider(8080)]` or `#[provider("mysql://...", env = "DB_URL")]`
    ///
    /// The macro generates a `Default` impl using this value.
    Value {
        /// The default value expression (numeric literal, string literal, or complex expr).
        default_value: syn::Expr,
        /// Optional environment variable name that overrides the default at runtime.
        env: Option<syn::LitStr>,
    },
}

/// Parses the token stream inside `#[provider(...)]`.
///
/// Grammar:
///   - Empty
///   - `TemplateIdent` [`(` Type `)`] [`,` NamedArg]*
///   - Literal [`,` NamedArg]*
///
/// Named args: `env = "..."`, `capacity = N`
impl Parse for ProviderArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Empty attributes: #[provider]
        if input.is_empty() {
            return Ok(ProviderArgs::Empty);
        }

        // Branch 1: Identifier — could be a template name
        if input.peek(Ident) {
            let ident: Ident = input.fork().parse()?;

            if is_template_name(&ident) {
                // Consume the identifier from the real stream
                let name: Ident = input.parse()?;

                // Check for parenthesized inner type: Queue(String)
                let inner_type = if input.peek(syn::token::Paren) {
                    let content;
                    syn::parenthesized!(content in input);
                    Some(content.parse::<syn::Type>()?)
                } else {
                    None
                };

                // Parse optional trailing named arguments
                let mut capacity = None;
                while input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                    if input.is_empty() {
                        break;
                    }

                    let key: Ident = input.parse()?;
                    input.parse::<Token![=]>()?;

                    match key.to_string().as_str() {
                        "capacity" => {
                            let lit: syn::LitInt = input.parse()?;
                            capacity = Some(lit.base10_parse::<usize>()?);
                        }
                        other => {
                            return Err(syn::Error::new(
                                key.span(),
                                format!(
                                    "Unknown template attribute '{}'. Supported: capacity",
                                    other
                                ),
                            ));
                        }
                    }
                }

                return Ok(ProviderArgs::Template {
                    name,
                    inner_type,
                    capacity,
                });
            }

            // Not a template name — fall through to treat as an expression
            // (e.g., a constant identifier used as a default value)
        }

        // Branch 2: Named-arg-only — e.g., `#[provider(env = "API_KEY")]`
        // Detect `Ident =` pattern to avoid misinterpreting it as an expression.
        if input.peek(Ident) && input.peek2(Token![=]) {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "env" => {
                    let env_lit = input.parse::<syn::LitStr>()?;
                    // env-only provider: no default value, env var is required at runtime
                    return Ok(ProviderArgs::Value {
                        default_value: syn::parse_quote!(""),
                        env: Some(env_lit),
                    });
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("Unknown provider attribute '{}'. Supported: env", other),
                    ));
                }
            }
        }

        // Branch 3: Literal or expression — default value
        let default_value: syn::Expr = input.parse()?;

        // Parse optional trailing named arguments (env)
        let mut env = None;
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
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("Unknown provider attribute '{}'. Supported: env", other),
                    ));
                }
            }
        }

        Ok(ProviderArgs::Value { default_value, env })
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
        assert!(matches!(args, ProviderArgs::Empty));
    }

    // ── Template branch ─────────────────────────────────────────────────

    #[test]
    fn notify_template_without_inner_type() {
        let args = parse_args(quote! { Notify }).unwrap();
        match args {
            ProviderArgs::Template {
                name,
                inner_type,
                capacity,
            } => {
                assert_eq!(name.to_string(), "Notify");
                assert!(inner_type.is_none());
                assert!(capacity.is_none());
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn event_template_is_recognized() {
        let args = parse_args(quote! { Event }).unwrap();
        assert!(matches!(args, ProviderArgs::Template { name, .. } if name == "Event"));
    }

    #[test]
    fn queue_template_with_inner_type() {
        let args = parse_args(quote! { Queue(String) }).unwrap();
        match args {
            ProviderArgs::Template {
                name, inner_type, ..
            } => {
                assert_eq!(name.to_string(), "Queue");
                assert!(
                    inner_type.is_some(),
                    "inner_type should be Some for Queue(String)"
                );
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn queue_template_with_capacity() {
        let args = parse_args(quote! { Queue(String), capacity = 500 }).unwrap();
        match args {
            ProviderArgs::Template { capacity, .. } => {
                assert_eq!(capacity, Some(500));
            }
            _ => panic!("Expected Template variant"),
        }
    }

    #[test]
    fn bqueue_alias_is_recognized() {
        let args = parse_args(quote! { BQueue(i32) }).unwrap();
        assert!(matches!(args, ProviderArgs::Template { name, .. } if name == "BQueue"));
    }

    // ── Value branch ────────────────────────────────────────────────────

    #[test]
    fn integer_literal_default() {
        let args = parse_args(quote! { 8080 }).unwrap();
        match args {
            ProviderArgs::Value { env, .. } => {
                assert!(env.is_none());
            }
            _ => panic!("Expected Value variant"),
        }
    }

    #[test]
    fn string_literal_default() {
        let args = parse_args(quote! { "mysql://localhost" }).unwrap();
        assert!(matches!(args, ProviderArgs::Value { .. }));
    }

    #[test]
    fn string_default_with_env() {
        let args = parse_args(quote! { "fallback", env = "MY_VAR" }).unwrap();
        match args {
            ProviderArgs::Value {
                env: Some(env_lit), ..
            } => {
                assert_eq!(env_lit.value(), "MY_VAR");
            }
            _ => panic!("Expected Value variant with env"),
        }
    }

    // ── Named-arg-only branch ───────────────────────────────────────────

    #[test]
    fn env_only_without_default() {
        let args = parse_args(quote! { env = "API_KEY" }).unwrap();
        match args {
            ProviderArgs::Value {
                env: Some(env_lit), ..
            } => {
                assert_eq!(env_lit.value(), "API_KEY");
            }
            _ => panic!("Expected Value variant with env-only"),
        }
    }

    // ── Unknown ident falls through to expression ───────────────────────

    #[test]
    fn unknown_ident_treated_as_expression() {
        // A non-template ident (e.g., a constant) should parse as a Value expression
        let args = parse_args(quote! { MY_CONST }).unwrap();
        assert!(matches!(args, ProviderArgs::Value { .. }));
    }

    // ── Error cases ─────────────────────────────────────────────────────

    #[test]
    fn unknown_template_attribute_is_error() {
        let result = parse_args(quote! { Queue(String), bogus = 42 });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown template attribute"),
            "Error message should mention unknown template attribute, got: {}",
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
}

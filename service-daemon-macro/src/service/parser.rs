//! Parser for `#[service]` macro attributes.

use quote::quote;
use syn::Token;
use syn::parse::Parse;

use crate::common::{TagsList, parse_scheduling_policy};

/// Parsed result of `#[service(...)]` attributes.
///
/// Supports the following syntax:
/// ```ignore
/// #[service]                                        // all defaults
/// #[service(priority = 80)]                         // priority only
/// #[service(tags = ["infra", "core"])]              // tags only
/// #[service(priority = 80, tags = ["infra"])]       // both
/// ```
#[derive(Debug)]
pub struct ServiceAttr {
    pub priority: proc_macro2::TokenStream,
    pub scheduling: proc_macro2::TokenStream,
    pub tags: proc_macro2::TokenStream,
}

impl Parse for ServiceAttr {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut priority: proc_macro2::TokenStream = quote!(50);
        let mut scheduling: proc_macro2::TokenStream =
            quote!(service_daemon::ServiceScheduling::Standard);
        let mut tags: proc_macro2::TokenStream = quote!(&[]);

        // Parse comma-separated key=value pairs
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "priority" => {
                    let value: syn::Expr = input.parse()?;
                    priority = quote!(#value);
                }
                "scheduling" => {
                    let ident: syn::Ident = input.parse()?;
                    scheduling = parse_scheduling_policy(&ident)?;
                }
                "tags" => {
                    let tag_list: TagsList = input.parse()?;
                    tags = tag_list.to_tokens();
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "Unknown service attribute '{}'. Supported: priority, scheduling, tags",
                            other
                        ),
                    ));
                }
            }

            // Consume optional trailing comma
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(ServiceAttr {
            priority,
            scheduling,
            tags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_str;

    #[test]
    fn test_parse_empty_attr() {
        let attr: ServiceAttr = parse_str("").unwrap();
        assert_eq!(attr.priority.to_string(), "50");
        assert_eq!(
            attr.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Standard"
        );
        assert_eq!(attr.tags.to_string(), "& []");
    }

    #[test]
    fn test_parse_priority_only() {
        let attr: ServiceAttr = parse_str("priority = 100").unwrap();
        assert_eq!(attr.priority.to_string(), "100");
    }

    #[test]
    fn test_parse_scheduling_isolated() {
        let attr: ServiceAttr = parse_str("scheduling = Isolated").unwrap();
        assert_eq!(
            attr.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Isolated"
        );
    }

    #[test]
    fn test_parse_scheduling_high_priority() {
        let attr: ServiceAttr = parse_str("scheduling = HighPriority").unwrap();
        assert_eq!(
            attr.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: HighPriority"
        );
    }

    #[test]
    fn test_parse_tags_only() {
        let attr: ServiceAttr = parse_str("tags = [\"a\", \"b\"]").unwrap();
        assert_eq!(attr.tags.to_string(), "& [\"a\" , \"b\"]");
    }

    #[test]
    fn test_parse_mixed_attributes() {
        let attr: ServiceAttr =
            parse_str("priority = 10, scheduling = Isolated, tags = [\"test\"]").unwrap();
        assert_eq!(attr.priority.to_string(), "10");
        assert_eq!(
            attr.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Isolated"
        );
        assert_eq!(attr.tags.to_string(), "& [\"test\"]");
    }

    #[test]
    fn test_parse_unknown_attribute() {
        let result: Result<ServiceAttr, syn::Error> = parse_str("unknown = true");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Unknown service attribute 'unknown'. Supported: priority, scheduling, tags"
        );
    }
}

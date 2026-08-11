//! Parser for `#[service]` macro attributes.

use crate::common::CommonEntryAttrs;

/// Parsed result of `#[service(...)]` attributes.
///
/// Supports the following syntax:
/// ```ignore
/// #[service]                                        // all defaults
/// #[service(priority = 80)]                         // priority only
/// #[service(scheduling = Isolated)]                  // scheduling only
/// #[service(auto_start = false)]                     // selected but not auto-started
/// #[service(tags = ["infra", "core"])]              // tags only
/// #[service(priority = 80, scheduling = HighPriority, auto_start = true, tags = ["infra"])]
/// ```
pub type ServiceAttr = CommonEntryAttrs;

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
        assert_eq!(attr.auto_start.to_string(), "true");
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
    fn test_parse_auto_start_false() {
        let attr: ServiceAttr = parse_str("auto_start = false").unwrap();
        assert_eq!(attr.auto_start.to_string(), "false");
    }

    #[test]
    fn test_parse_mixed_attributes() {
        let attr: ServiceAttr = parse_str(
            "priority = 10, scheduling = Isolated, auto_start = false, tags = [\"test\"]",
        )
        .unwrap();
        assert_eq!(attr.priority.to_string(), "10");
        assert_eq!(
            attr.scheduling.to_string(),
            "service_daemon :: ServiceScheduling :: Isolated"
        );
        assert_eq!(attr.auto_start.to_string(), "false");
        assert_eq!(attr.tags.to_string(), "& [\"test\"]");
    }

    #[test]
    fn test_parse_unknown_attribute() {
        let result: Result<ServiceAttr, syn::Error> = parse_str("unknown = true");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Unknown service attribute 'unknown'. Supported: priority, scheduling, auto_start, tags"
        );
    }
}

//! Attribute parsing for the `#[trigger]` macro.

use std::collections::HashMap;

/// Parses trigger attributes from the attribute string.
pub fn parse_attrs(attr_str: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in attr_str.split(',') {
        if let Some((key, val)) = part.split_once('=') {
            map.insert(key.trim().to_string(), val.trim().to_string());
        }
    }
    map
}

/// The list of valid trigger template variants.
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

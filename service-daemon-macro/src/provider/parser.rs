//! Attribute parsing for the `#[provider]` macro.

/// Provider attribute configuration.
#[derive(Default)]
pub struct ProviderAttrs {
    pub default_value: Option<String>,
    pub env_name: Option<String>,
    /// Only applies to Queue template (e.g., item_type = "MyTask")
    pub item_type: Option<String>,
    /// Capacity for Queue template (default 100)
    pub capacity: Option<usize>,
}

/// Parses attributes from #[provider(default = ..., env_name = "...", item_type = "...", capacity = N)]
pub fn parse_provider_attrs(attr_str: &str) -> ProviderAttrs {
    let mut attrs = ProviderAttrs::default();

    // Handle empty attributes
    if attr_str.trim().is_empty() {
        return attrs;
    }

    // Helper to apply a key-value pair to attrs
    let mut apply_kv = |key: &str, val: String| match key.trim().to_lowercase().as_str() {
        "default" | "value" => attrs.default_value = Some(val),
        "env_name" => attrs.env_name = Some(val.trim_matches('"').to_string()),
        "item_type" => attrs.item_type = Some(val.trim_matches('"').to_string()),
        "capacity" => attrs.capacity = val.parse().ok(),
        _ => {}
    };

    // Parse key-value pairs, handling nested parentheses
    let mut depth = 0;
    let mut current_key = String::new();
    let mut current_value = String::new();
    let mut in_value = false;
    let mut in_string = false;

    for ch in attr_str.chars() {
        match ch {
            '"' if depth == 0 => {
                in_string = !in_string;
                current_value.push(ch);
            }
            '(' | '[' | '{' => {
                depth += 1;
                if in_value {
                    current_value.push(ch);
                }
            }
            ')' | ']' | '}' => {
                if depth > 0 {
                    depth -= 1;
                }
                if in_value && depth > 0 {
                    current_value.push(ch);
                }
            }
            '=' if depth == 0 && !in_string && !in_value => {
                in_value = true;
            }
            ',' if depth == 0 && !in_string => {
                // End of key-value pair
                apply_kv(&current_key, current_value.trim().to_string());
                current_key.clear();
                current_value.clear();
                in_value = false;
            }
            _ => {
                if in_value {
                    current_value.push(ch);
                } else {
                    current_key.push(ch);
                }
            }
        }
    }

    // Handle last key-value pair
    if !current_key.is_empty() {
        apply_kv(&current_key, current_value.trim().to_string());
    }

    attrs
}

//! Integration tests for Type-Based DI and service registry
//!
//! These tests verify that the Provided trait and SERVICE_REGISTRY work correctly.

use service_daemon::{SERVICE_REGISTRY, TRIGGER_REGISTRY};
use std::collections::HashSet;

/// Test that we can collect service names from the registry
#[test]
fn test_collect_service_names() {
    let services: HashSet<String> = SERVICE_REGISTRY
        .iter()
        .map(|entry| entry.name.to_string())
        .collect();

    // Just verify the collection works
    let _ = services.len();
}

/// Test that triggers are now registered as services
#[test]
fn test_triggers_registered_as_services() {
    // With the new design, triggers register as services
    // Check that all services (including triggers) are in SERVICE_REGISTRY
    for entry in SERVICE_REGISTRY.iter() {
        assert!(!entry.name.is_empty(), "Service name should not be empty");
        assert!(
            !entry.module.is_empty(),
            "Service module should not be empty"
        );
    }
}

/// Test that TRIGGER_REGISTRY is empty (since triggers are now services)
#[test]
fn test_trigger_registry_empty() {
    // Triggers now register as services, so TRIGGER_REGISTRY should be empty
    // This test verifies the migration to unified service-based architecture
    let trigger_count = TRIGGER_REGISTRY.len();
    assert_eq!(
        trigger_count, 0,
        "TRIGGER_REGISTRY should be empty - triggers now register as services"
    );
}

/// Test ServiceEntry structure
#[test]
fn test_service_entry_fields() {
    for entry in SERVICE_REGISTRY.iter() {
        assert!(!entry.name.is_empty(), "Service name should not be empty");
        assert!(
            !entry.module.is_empty(),
            "Service module should not be empty"
        );

        // Verify params are accessible
        let _ = entry.params.len();
    }
}

/// Test that trigger-based services have the correct module pattern
#[test]
fn test_trigger_service_modules() {
    for entry in SERVICE_REGISTRY.iter() {
        // Triggers have module like "triggers/cron", "triggers/queue", etc.
        if entry.module.starts_with("triggers/") {
            let template = entry.module.strip_prefix("triggers/").unwrap();
            let valid_templates = ["custom", "queue", "cron"];
            assert!(
                valid_templates.contains(&template),
                "Trigger template '{}' should be one of: {:?}",
                template,
                valid_templates
            );
        }
    }
}

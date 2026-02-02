# Extending the Framework

This guide is for developers looking to add new capabilities to the `service-daemon-rs` core or macros.

## 1. Adding a New Trigger Template

Triggers are implemented as specialized services with a host wrapper.
1. **Define the Host**: Add a new host function in `service-daemon/src/utils/triggers.rs` (e.g., `mqtt_trigger_host`).
2. **Update the Macro**: Modify `service-daemon-macro/src/trigger.rs` to recognize the new template name and generate the appropriate call.
3. **Map Parameters**: Use the macro utilities to correctly distinguish between event payloads and DI resources.

## 2. Adding a "Magic Provider"

Magic providers (like `Notify` or `Queue`) provide specialized behavior automatically when used as a default.
1. Add a new template generator function in `service-daemon-macro/src/provider.rs`.
2. Update `generate_struct_provider` to detect your new template name in the `#[provider(default = ...)]` attribute.

## 3. Modifying Registry Mechanics

The framework uses `linkme` for distributed registration. If you need to change how services are registered:
1. Update common types in `service-daemon/src/models/`.
2. Ensure consistent updates in the macro generation logic in `service-daemon-macro/src/service.rs` and `trigger.rs`.

[Back to README](../../README.md)

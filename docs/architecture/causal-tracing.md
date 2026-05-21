# Causal tracing in asynchronous chains

In an event-driven system, one trigger handler can fan out, enqueue work, or publish follow-up events. Parent-child spans alone do not fully describe that relationship once execution crosses queues or service boundaries. `service-daemon-rs` records a small causal identity on each event so logs and diagnostics can reconstruct the chain.

## 1. Event identity

Each event carries the fields needed to identify where it came from and which handler is processing it:

1. **MessageId** (`Uuid` v7): a time-ordered unique ID for the event.
2. **SourceId** (`ServiceId` / `usize`): the service that originally published the event.
3. **Instance identity**:
   - **ServiceId**: the service currently handling the event.
   - **InstanceSeq** (`u64`): a monotonic sequence number for the current trigger invocation.

Together, `ServiceId` and `InstanceSeq` form the **InstanceId**, a compact numeric identity for one trigger invocation.

## 2. Propagation

When a service runs, the `ServiceSupervisor` creates a `tracing::Span` with the service identity. When a trigger handler runs, the `TriggerRunner` creates a nested span with the incoming `message_id` and the current `instance_seq`. `DaemonLayer` reads those span fields and attaches them to emitted log events.

If handler B publishes event Y while handling event X, event Y gets a new UUID v7 `message_id` and keeps the original `SourceId`. This lets diagnostics link a cascade of events back to the service that started it, even when the work crosses services or queues.

## 3. Why this matters

The causal identity lets diagnostics:

- group side effects that came from the same original event;
- identify the service that started a multi-hop chain;
- connect logs and trigger executions without relying on string-based correlation IDs.

[Back to README](../../README.md)

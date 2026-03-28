# The Ripple Model: Causal Tracing in Asynchronous Chains

In highly decoupled, event-driven systems like `service-daemon-rs`, traditional linear tracing (like OpenTelemetry spans) often fails to capture the "why" behind complex asynchronous interactions. We solve this using the **Ripple Model**.

## 1. The Analogy: A Stone in the Water
Imagine a stone thrown into a still pond. The stone creates ripples that spread outward, potentially reaching distant shores and causing other stones to move. 

- **The Stone**: A `TriggerMessage` (an event).
- **The Ripples**: Trigger handlers executing in response.
- **The Secondary Stones**: New events published by those handlers.

## 2. The Mechanics of Causality

Every event in the framework carries a `TriggerContext` composed of:

1.  **MessageId**: A globally unique **UUID v7** (time-ordered). This ensures high-performance causal tracking with zero collision risk and natural temporal sorting.
2.  **SourceId**: The `ServiceId` of the service instance that **originally** published the event.
3.  **InstanceId**: A stack-allocated numeric composite (`ServiceId` + `u64` sequence). This identifies the specific trigger invocation without requiring heap-allocated strings.

### Forward Propagation
When a service runs, the `ServiceSupervisor` creates a `tracing::Span` carrying the service's name. When a trigger handler fires, the `TriggerRunner` creates a nested Span carrying `message_id` (UUID) and the numeric `instance_id` components. Any log message emitted within these Spans is automatically decorated with these IDs by the `DaemonLayer`.

### Causal Linking
If Handler B publishes a new Event Y in response to Event X, Event Y **inherits** the `SourceId` of the original initiator (the stone), but gets its own unique UUID v7 `message_id`. This allows the **Topology Collector** to trace an entire cascade of events back to a single root cause, even if they cross multiple service boundaries and logical "waves".

## 3. Why this matters: The "Echo" Problem
In traditional systems, if Service A pings Service B, which then pings Service C, the logs look like a straight line. But in our reactive model, one event might trigger 10 different handlers simultaneously. 

The Ripple Model allows you to:
- **Trace the Cascade**: See all 10 side-effects of a single state change through the automated topology map.
- **Identify the Originator**: Even if an error happens 5 hops away, the `SourceId` points directly to the service that started the chain.
- **Zero-Allocation Tracking**: By using UUIDs and numeric components for `InstanceId`, the entire tracing pipeline carries zero heap-allocation overhead in the hot path. All context is handled by the `TracingInterceptor` pipeline, requiring zero manual boilerplate from the developer.

[Back to README](../../README.md)

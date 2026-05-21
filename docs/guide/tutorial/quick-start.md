# Quick Start Guide

This tutorial walks through `service-daemon-rs` from a single heartbeat service to event-driven triggers, state recovery, priority scheduling, and unit testing. It assumes basic familiarity with `tokio` and async Rust.

The framework manages the lifecycle of long-running async tasks (start order, runtime placement, restarts, dependencies, signal handling) and provides type-driven dependency injection. The chapters below introduce these one at a time.

---

## What are we building?

In this tutorial, we will walk through the progression of a typical service-daemon application:

1.  [**First Service**](./first-service.md) -- Your first service. We'll learn the basics of defining and running a background task.
2.  [**Reactive Triggers**](./reactive-triggers.md) -- Events, queues, and automation. Learn how to react to external events.
3.  [**State Management & Recovery**](./state-recovery.md) -- Persistence and resilience. See how services restore state after failure.
4.  [**Custom Providers**](./custom-providers.md) -- Integrating external systems like MQTT or databases.
5.  [**Error Handling & Retries**](./error-handling.md) -- Learn exponential backoff and fatal-error stop behavior.
6.  [**Priorities & Scheduling Policies**](./priority-orchestration.md) -- Manage initialization order and choose the right execution mode.
7.  [**Unit Testing & Simulation**](./unit-testing.md) -- Test your logic in a controlled sandbox with `MockContext`.

---

## Reading order

The chapters build on each other; reading in order is recommended for the first pass. This directory is the beginner tutorial path: it focuses on best-practice usage, not framework internals or maintainer extension points.

After finishing the tutorial, return to the [README](../../../README.md) documentation section for complete user guides, architecture references, and maintainer material.

[**Next: First Service ->**](./first-service.md)

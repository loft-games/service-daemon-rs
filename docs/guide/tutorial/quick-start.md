# Quick Start Guide

Welcome to the `service-daemon-rs` tutorial! If you've ever felt like building a reliable, event-driven background system is a challenge due to race conditions, thread locks, and complex lifecycle management, you're in the right place.

`service-daemon-rs` is designed for **Boilerplate-free Orchestration**. It handles service management, dependency injection, and recovery, so you can focus on writing your business logic.

---

## What are we building?

In this tutorial, we will walk through the progression of a real-world system:

1.  [**Hello, Heartbeat!**](./hello-heartbeat.md) -- Your first service. We'll learn the basics of defining and running a background task.
2.  [**Reactive Triggers**](./reactive-triggers.md) -- Events, queues, and automation. Learn how to make your system react to the world.
3.  [**State Management & Recovery**](./state-recovery.md) -- Persistence and resilience. See how services survive failures and restore state.
4.  [**DIY Providers**](./diy-providers.md) -- Integrating external systems like MQTT or Databases.
5.  [**Error Handling & Retries**](./error-handling.md) -- Learn exponential backoff and the "Kill Switch" for fatal errors.
6.  [**Priorities & Scheduling Policies**](./priority-orchestration.md) -- Managing initialization order and choosing between the shared standard runtime, the shared high-priority runtime, and isolated execution.
7.  [**Unit Testing & Simulation**](./unit-testing.md) -- Test your logic in a controlled sandbox (MockContext).

---

## Ready to dive in?

We recommend following the chapters in order, but feel free to jump around if you're looking for specific solutions.

[**Start the Journey: Hello, Heartbeat! ->**](./hello-heartbeat.md)

### Additional Chapters

-  [**Custom Trigger Implementation**](./tailor-made-triggers.md) -- Build your own trigger types from scratch.
-  [**Trigger Middlewares (Interceptors)**](./trigger-interceptors.md) -- Add custom middleware to the trigger dispatch pipeline.
-  [**Advanced Macro Usage**](./advanced-macros.md) -- Deep dive into the `#[service]` and `#[trigger]` macros.

# Quick Start Guide

Welcome to the `service-daemon-rs` tutorial! If you've ever felt like building a reliable, event-driven background system is a challenge due to race conditions, thread locks, and complex lifecycle management, you're in the right place.

`service-daemon-rs` is designed for **zero-boilerplate**. It handles service orchestration, dependency injection, and recovery, so you can focus on writing your business logic.

---

## What are we building?

In this tutorial, we will walk through the progression of a real-world system:

1.  [**Hello, Heartbeat!**](docs/guide/tutorial/hello-heartbeat.md) -- Your first service. We'll learn the basics of defining and running a background task.
2.  [**Reactive Triggers**](docs/guide/tutorial/reactive-triggers.md) -- Events, queues, and automation. Learn how to make your system react to the world.
3.  [**State Management & Recovery**](docs/guide/tutorial/state-recovery.md) -- Persistence and resilience. See how services survive failures and restore state.
4.  [**DIY Providers**](docs/guide/tutorial/diy-providers.md) -- Integrating external systems like MQTT or Databases.
5.  [**Error Handling & Retries**](docs/guide/tutorial/error-handling.md) -- Learn exponential backoff and the "Kill Switch" for fatal errors.
6.  [**Sequential Startup & Shutdown**](docs/guide/tutorial/priority-orchestration.md) -- Managing initialization order with priorities.
7.  [**Unit Testing & Simulation**](docs/guide/tutorial/unit-testing.md) -- Test your logic in a controlled sandbox (MockContext).

---

## Ready to dive in?

We recommend following the chapters in order, but feel free to jump around if you're looking for specific solutions.

[**Start the Journey: Hello, Heartbeat! ->**](docs/guide/tutorial/hello-heartbeat.md)

### Additional Chapters

-  [**Custom Trigger Implementation**](docs/guide/tutorial/tailor-made-triggers.md) -- Build your own trigger types from scratch.
-  [**Trigger Middlewares (Interceptors)**](docs/guide/tutorial/trigger-interceptors.md) -- Add custom middleware to the trigger dispatch pipeline.
-  [**Advanced Macro Usage**](docs/guide/tutorial/advanced-macros.md) -- Deep dive into the `#[service]` and `#[trigger]` macros.

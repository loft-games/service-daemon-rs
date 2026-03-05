# The Grand Tour

Welcome to the `service-daemon-rs` tutorial! If you've ever felt like building a reliable, event-driven background system is a constant battle against race conditions, thread locks, and complex lifecycle management, you're in the right place.

`service-daemon-rs` is designed to be **invisible**. It handles the heavy lifting of service orchestration, dependency injection, and recovery, so you can focus on writing your business logic.

---

## What are we building?

In this tutorial, we will walk through the progression of a real-world system:

1.  [**Hello, Heartbeat!**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/hello-heartbeat.md) -- Your first service. We'll learn the basics of defining and running a background task.
2.  [**Reactive Triggers**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/reactive-triggers.md) -- Events, queues, and automation. Learn how to make your system react to the world.
3.  [**The Art of Recovery**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/art-of-recovery.md) -- Resilience by design. See how services survive failures and restore their soul (state).
4.  [**DIY Providers**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/diy-providers.md) -- Integrating external worlds like MQTT or Databases without the boilerplate.
5.  [**Resilience Kung-Fu**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/resilience-kung-fu.md) -- Master the exponential backoff and the "Kill Switch" for fatal errors.
6.  [**Waves of Orchestration**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/orchestration-waves.md) -- Directing the startup and shutdown symphony with priorities.
7.  [**Playing God: Simulator**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/playing-god.md) -- Test your logic in a controlled sandbox where you control time and state.

---

## Ready to dive in?

We recommend following the chapters in order, but feel free to jump around if you're looking for specific solutions.

[**Start the Journey: Hello, Heartbeat! ->**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/hello-heartbeat.md)

### Additional Chapters

-  [**Tailor-Made Triggers**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/tailor-made-triggers.md) -- Build your own trigger types from scratch.
-  [**The Interceptor Gauntlet**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/interceptor-gauntlet.md) -- Add custom middleware to the trigger dispatch pipeline.
-  [**Macro Magic Unleashed**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/macro-magic.md) -- Deep dive into the `#[service]` and `#[trigger]` macros.

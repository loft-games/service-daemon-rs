// Tutorial module -- re-exports content from docs/guide/tutorial/*.md
// so that `docs.rs` can render the full tutorial alongside the API reference.
//
// Each submodule uses `include_str!` to embed the corresponding Markdown file.
// The canonical source of truth remains the Markdown files under `docs/guide/tutorial/`.

//! > **For the best reading experience with full navigation, read the
//! > [Tutorial on GitHub](https://github.com/loft-games/service-daemon-rs/tree/master/docs/guide/tutorial).**
//!

#![doc = include_str!("../../docs/guide/tutorial/grand-tour.md")]

/// Your first service - learn the basics of Providers, Services, and the Daemon.
///
/// **Tutorial Chapter 1**
pub mod hello_heartbeat {
    #![doc = include_str!("../../docs/guide/tutorial/hello-heartbeat.md")]
}

/// Events, queues, and automation - make your system react to the world.
///
/// **Tutorial Chapter 2**
pub mod reactive_triggers {
    #![doc = include_str!("../../docs/guide/tutorial/reactive-triggers.md")]
}

/// Resilience by design - graceful migration and state recovery.
///
/// **Tutorial Chapter 3**
pub mod art_of_recovery {
    #![doc = include_str!("../../docs/guide/tutorial/art-of-recovery.md")]
}

/// Integrating external resources like MQTT or Databases without boilerplate.
///
/// **Tutorial Chapter 4**
pub mod diy_providers {
    #![doc = include_str!("../../docs/guide/tutorial/diy-providers.md")]
}

/// Master exponential backoff, scaling policies, and the "Kill Switch".
///
/// **Tutorial Chapter 5**
pub mod resilience_kung_fu {
    #![doc = include_str!("../../docs/guide/tutorial/resilience-kung-fu.md")]
}

/// Directing the startup and shutdown symphony with priorities.
///
/// **Tutorial Chapter 6**
pub mod orchestration_waves {
    #![doc = include_str!("../../docs/guide/tutorial/orchestration-waves.md")]
}

/// Test your logic in a controlled sandbox where you control time and state.
///
/// **Tutorial Chapter 7**
pub mod playing_god {
    #![doc = include_str!("../../docs/guide/tutorial/playing-god.md")]
}

/// Peek behind the curtain at the Registry, Runner, Status Plane, and Shelf.
///
/// **Bonus Chapter**
pub mod under_the_hood {
    #![doc = include_str!("../../docs/guide/tutorial/under-the-hood.md")]
}

/// Build your own trigger types from scratch using the `TriggerHost<T>` trait.
///
/// **Bonus Chapter**
pub mod tailor_made_triggers {
    #![doc = include_str!("../../docs/guide/tutorial/tailor-made-triggers.md")]
}

/// Add custom middleware to the trigger dispatch pipeline.
///
/// **Bonus Chapter**
pub mod interceptor_gauntlet {
    #![doc = include_str!("../../docs/guide/tutorial/interceptor-gauntlet.md")]
}

/// Deep dive into the `#[service]` and `#[trigger]` macros.
///
/// **Bonus Chapter**
pub mod macro_magic {
    #![doc = include_str!("../../docs/guide/tutorial/macro-magic.md")]
}

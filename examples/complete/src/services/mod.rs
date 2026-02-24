//! Service definitions for the complete example.
//!
//! All services here use the `loop { match state() { ... } }` pattern
//! for explicit lifecycle management. This is the **advanced pattern**
//! for services that need fine-grained control over state transitions.

pub mod lifecycle;

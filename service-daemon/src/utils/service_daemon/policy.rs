//! Restart policy configuration for service recovery.

use rand::Rng;
use std::time::Duration;

/// Configuration for service restart behavior with exponential backoff.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    /// Initial delay before first restart (default: 1 second)
    pub initial_delay: Duration,
    /// Maximum delay between restarts (default: 5 minutes)
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (default: 2.0)
    pub multiplier: f64,
    /// Delay resets to initial after this duration of successful running (default: 60 seconds)
    pub reset_after: Duration,
    /// Jitter factor (0.0 to 1.0) - randomizes delay to prevent thundering herd (default: 0.1)
    pub jitter_factor: f64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300), // 5 minutes
            multiplier: 2.0,
            reset_after: Duration::from_secs(60),
            jitter_factor: 0.1, // 10% jitter by default
        }
    }
}

impl RestartPolicy {
    /// Create a restart policy builder.
    pub fn builder() -> RestartPolicyBuilder {
        RestartPolicyBuilder::default()
    }

    /// Create a restart policy for testing with shorter delays.
    pub fn for_testing() -> Self {
        Self {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            multiplier: 2.0,
            reset_after: Duration::from_secs(5),
            jitter_factor: 0.0, // No jitter for predictable tests
        }
    }

    /// Calculate the next restart delay using exponential backoff with jitter.
    pub fn next_delay(&self, current_delay: Duration) -> Duration {
        let base = current_delay.as_secs_f64() * self.multiplier;
        let jitter_range = base * self.jitter_factor;
        let jitter = rand::rng().random_range(-jitter_range..=jitter_range);
        let next = Duration::from_secs_f64((base + jitter).max(0.0));
        next.min(self.max_delay)
    }
}

/// Builder for `RestartPolicy`.
#[derive(Default)]
pub struct RestartPolicyBuilder {
    policy: RestartPolicy,
}

impl RestartPolicyBuilder {
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.policy.initial_delay = delay;
        self
    }

    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.policy.max_delay = delay;
        self
    }

    pub fn multiplier(mut self, multiplier: f64) -> Self {
        self.policy.multiplier = multiplier;
        self
    }

    pub fn reset_after(mut self, duration: Duration) -> Self {
        self.policy.reset_after = duration;
        self
    }

    pub fn jitter_factor(mut self, factor: f64) -> Self {
        self.policy.jitter_factor = factor.clamp(0.0, 1.0);
        self
    }

    #[must_use]
    pub fn build(self) -> RestartPolicy {
        self.policy
    }
}

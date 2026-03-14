//! Pass case: A provider struct with `Arc<RwLock<T>>` and `Arc<Mutex<T>>` fields
//! should automatically inject using `ManagedProvided` (`resolve_rwlock()` / `resolve_mutex()`).

use service_daemon::core::managed_state::{Mutex, RwLock};
use service_daemon::provider;
use std::sync::Arc;

#[derive(Clone)]
#[provider(42)]
pub struct Counter(pub i32);

#[derive(Clone)]
#[provider("admin")]
pub struct Username(pub String);

/// A composite provider that demonstrates automatic RwLock/Mutex injection.
/// The macro should detect the wrapper types and generate the correct DI calls.
#[derive(Clone)]
#[provider]
pub struct MutableState {
    pub counter: Arc<RwLock<Counter>>,
    pub username: Arc<Mutex<Username>>,
}

fn main() {}

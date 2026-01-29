use linkme::distributed_slice;

/// A mark that indicates a type requires mutable access via Arc<RwLock<T>> or Arc<Mutex<T>>.
/// This is used for intelligent promotion from OnceCell to StateManager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutabilityMark {
    /// The key identifies the type (typically normalized type name).
    pub key: &'static str,
}

/// Registry of all mutability marks found in the application at link time.
#[distributed_slice]
pub static MUTABILITY_REGISTRY: [MutabilityMark];

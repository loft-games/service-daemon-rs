use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::any::{Any, TypeId};
use std::sync::Arc;

pub struct Container {
    items: DashMap<(TypeId, String), Arc<dyn Any + Send + Sync>>,
}

impl Container {
    pub fn new() -> Self {
        Self {
            items: DashMap::new(),
        }
    }

    pub fn register<T: Any + Send + Sync>(&self, name: &str, item: T) {
        self.items
            .insert((TypeId::of::<T>(), name.to_string()), Arc::new(item));
    }

    #[allow(dead_code)]
    pub fn register_arc<T: Any + Send + Sync>(&self, name: &str, item: Arc<T>) {
        self.items
            .insert((TypeId::of::<T>(), name.to_string()), item);
    }

    pub fn resolve<T: Any + Send + Sync>(&self, name: &str) -> Option<Arc<T>> {
        self.items
            .get(&(TypeId::of::<T>(), name.to_string()))
            .map(|val| {
                val.value()
                    .clone()
                    .downcast::<T>()
                    .expect("Type mismatch in container: name exists but type is different")
            })
    }

    /// Clear all entries from the container (useful for test isolation)
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.items.clear();
    }
}

pub static GLOBAL_CONTAINER: Lazy<Container> = Lazy::new(Container::new);

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::debug;

    fn setup_tracing() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_register_and_resolve_i32() {
        setup_tracing();
        let container = Container::new();
        container.register("port", 8080i32);

        let resolved = container.resolve::<i32>("port");
        assert!(resolved.is_some());
        assert_eq!(*resolved.unwrap(), 8080);
        debug!("test_register_and_resolve_i32 passed");
    }

    #[test]
    fn test_register_and_resolve_string() {
        setup_tracing();
        let container = Container::new();
        container.register("db_url", "mysql://localhost".to_string());

        let resolved = container.resolve::<String>("db_url");
        assert!(resolved.is_some());
        assert_eq!(*resolved.unwrap(), "mysql://localhost");
        debug!("test_register_and_resolve_string passed");
    }

    #[test]
    fn test_resolve_nonexistent_returns_none() {
        setup_tracing();
        let container = Container::new();

        let resolved = container.resolve::<i32>("nonexistent");
        assert!(resolved.is_none());
        debug!("test_resolve_nonexistent_returns_none passed");
    }

    #[test]
    fn test_same_name_different_types() {
        setup_tracing();
        let container = Container::new();
        container.register("value", 42i32);
        container.register("value", "hello".to_string());

        let int_val = container.resolve::<i32>("value");
        let str_val = container.resolve::<String>("value");

        assert!(int_val.is_some());
        assert!(str_val.is_some());
        assert_eq!(*int_val.unwrap(), 42);
        assert_eq!(*str_val.unwrap(), "hello");
        debug!("test_same_name_different_types passed");
    }

    #[test]
    fn test_register_arc() {
        setup_tracing();
        let container = Container::new();
        let shared = Arc::new(vec![1, 2, 3]);
        container.register_arc("numbers", shared.clone());

        let resolved = container.resolve::<Vec<i32>>("numbers");
        assert!(resolved.is_some());
        assert_eq!(*resolved.unwrap(), vec![1, 2, 3]);
        debug!("test_register_arc passed");
    }

    #[test]
    fn test_clear() {
        setup_tracing();
        let container = Container::new();
        container.register("port", 8080i32);
        container.clear();

        let resolved = container.resolve::<i32>("port");
        assert!(resolved.is_none());
        debug!("test_clear passed");
    }
}

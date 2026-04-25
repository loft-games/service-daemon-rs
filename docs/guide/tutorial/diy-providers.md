# DIY Providers

In the first chapter, we used a `#[provider]` macro on a simple struct. Real applications often need more: a database connection pool, an MQTT client, an HTTP client with custom config.

For these, the struct + `Default` pattern doesn't fit -- initialization is async, fallible, or depends on configuration. Use a **Provider Function** instead.

---

## 1. The Async Provider Function

If your provider requires `async` setup (like connecting to a server), define a function marked with `#[provider]`.

```rust,ignore
use service_daemon::provider;
use rumqttc::{AsyncClient, MqttOptions};

pub struct MqttBus {
    pub client: AsyncClient,
}

#[provider]
async fn mqtt_bus_provider() -> MqttBus {
    let mut mqttoptions = MqttOptions::new("rumqtt-async", "localhost", 1883);
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
    
    // In a real app, you'd spawn the eventloop in a background task
    tokio::spawn(async move {
        while let Ok(_notification) = eventloop.poll().await {}
    });

    MqttBus { client }
}
```

## 2. Shared vs. Fresh Instances

By default, every service that asks for `Arc<MqttBus>` will receive the **same instance** (Singleton-like behavior). The framework calls your function once and caches the result.

## 3. Using Dependencies in Providers

Providers can depend on other providers! The framework handles the dependency graph for you.

```rust,ignore
#[provider]
pub struct DatabaseUrl(pub String);

#[provider]
async fn connection_pool_provider(url: Arc<DatabaseUrl>) -> MyDbPool {
    MyDbPool::connect(&url).await.expect("Failed to connect to DB")
}
```

## 4. Error Handling and Retries

Network resources may not be ready when your service starts. Instead of panicking, return a `Result<T, ProviderError>` and let the framework retry.

The framework provides two error types:
*   **`ProviderError::Fatal("msg")`**: Use this for configuration errors. The daemon will fail-fast and exit immediately.
*   **`ProviderError::Retryable("msg")`**: Use this for connectivity issues. The framework will automatically retry with exponential backoff until the `provider_init_timeout` is reached.

```rust
use service_daemon::{provider, ProviderError};

#[provider]
async fn fallible_db_provider(url: Arc<Url>) -> Result<MyDb, ProviderError> {
    MyDb::connect(&url).await.map_err(|e| {
        if is_transient(e) {
            ProviderError::Retryable(format!("DB not ready: {e}"))
        } else {
            ProviderError::Fatal(format!("Invalid DB config: {e}"))
        }
    })
}
```

## 5. Best Practices

*   **Keep it clean**: Use Providers for *Shared Resources* (DB, MQTT, Config). Use Services for *Action* (Running the business logic).
*   **Don't Block**: Always use `async` providers for network/disk operations.
*   **Fail Gracefully**: Prefer `ProviderError::Retryable` for network resources, so transient unavailability at startup (DB still booting, broker not yet listening) doesn't kill your daemon.

> [!TIP]
> **Deep Dive**: For complex naming conventions and advanced lifecycle patterns, see the [Provider Best Practices](../provider-best-practices.md) guide.

---

[**<- Previous Step: State Management & Recovery**](./state-recovery.md) | [**Next Step: Error Handling & Retries ->**](./error-handling.md)

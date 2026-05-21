# Custom Providers

In the first chapter, we used a `#[provider]` macro on a simple struct. Real applications often need more: a database connection pool, an MQTT client, an HTTP client with custom config.

For these, the struct + `Default` pattern doesn't fit -- initialization is async, fallible, or depends on configuration. Use a provider function instead.

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

By default, every service that asks for `Arc<MqttBus>` will receive the **same shared instance** for its effective provider scope. In ordinary apps that means the framework calls your function once and caches the result for reuse.

## 3. Using Dependencies in Providers

Providers can depend on other providers. The framework validates the provider dependency graph before startup.

```rust,ignore
use service_daemon::{provider, ProviderError};
use std::sync::Arc;

#[provider]
pub struct DatabaseUrl(pub String);

#[provider]
async fn connection_pool_provider(url: Arc<DatabaseUrl>) -> Result<MyDbPool, ProviderError> {
    MyDbPool::connect(&url)
        .await
        .map_err(|e| ProviderError::Retryable(format!("DB not ready: {e}")))
}
```

## 4. Error Handling and Retries

Network resources may not be ready when your service starts. Instead of panicking, return a `Result<T, ProviderError>` and let the framework retry.

The framework provides two error types:
*   **`ProviderError::Fatal("msg")`**: Use this for configuration errors. The daemon will fail-fast and exit immediately.
*   **`ProviderError::Retryable("msg")`**: Use this for connectivity issues. The framework will automatically retry with exponential backoff until the `provider_init_timeout` is reached.

```rust,ignore
use service_daemon::{provider, ProviderError};
use std::sync::Arc;

struct Url(String);
struct MyDb;

impl MyDb {
    async fn connect(_url: &Url) -> Result<Self, std::io::Error> {
        todo!("connect to your database")
    }
}

fn is_transient(_error: &std::io::Error) -> bool {
    true
}

#[provider]
async fn fallible_db_provider(url: Arc<Url>) -> Result<MyDb, ProviderError> {
    MyDb::connect(&url).await.map_err(|e| {
        if is_transient(&e) {
            ProviderError::Retryable(format!("DB not ready: {e}"))
        } else {
            ProviderError::Fatal(format!("Invalid DB config: {e}"))
        }
    })
}
```

## 5. Provider guidelines

* Use providers for shared resources such as database pools, MQTT clients, HTTP clients, and configuration.
* Keep network and disk initialization in `async` provider functions.
* Return `ProviderError::Retryable` for transient startup failures so the daemon can retry within the configured provider initialization timeout.

> [!TIP]
> **Deep Dive**: For complex naming conventions and advanced lifecycle patterns, see the [Provider Best Practices](../provider-best-practices.md) guide.

---

[**<- Previous Step: State Management & Recovery**](./state-recovery.md) | [**Next Step: Error Handling & Retries ->**](./error-handling.md)

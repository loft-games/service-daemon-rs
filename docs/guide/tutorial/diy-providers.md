# DIY Providers

In the first chapter, we used a `#[provider]` macro on a simple struct. But world-class applications often need more: a database connection pool, an MQTT client, or a complex HTTP client.

You shouldn't put complex initialization logic inside a macro. Instead, you can define a **Provider Function**.

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

## 4. Best Practices

*   **Keep it clean**: Use Providers for *Shared Resources* (DB, MQTT, Config). Use Services for *Action* (Running the business logic).
*   **Don't Block**: Always use `async` providers for network/disk operations.
*   **Fail Fast**: If a provider cannot be initialized, use `.expect()` or `panic!`. The framework will catch this and report it as a startup error.

> [!TIP]
> **Deep Dive**: For complex naming conventions and advanced lifecycle patterns, see the [Provider Best Practices](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/provider-best-practices.md) guide.

---

[**<- Previous Step: The Art of Recovery**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/art-of-recovery.md) | [**Next Step: Resilience Kung-Fu ->**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/resilience-kung-fu.md)

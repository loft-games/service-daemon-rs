# Sequential Startup & Shutdown

In a large system, order matters. You can't start your API Gateway before your Database is ready, and you shouldn't shut down your metrics logger until everything else has finished reporting.

`service-daemon-rs` uses a **Wave-based** orchestration system driven by **Priorities**.

---

## 1. Setting Priorities

Every service and trigger has a priority. The default is `50`. High numbers mean "more important".

```rust,ignore
use service_daemon::ServicePriority;

#[service(priority = ServicePriority::SYSTEM)] // 100
async fn logger_service() { ... }

#[service(priority = ServicePriority::STORAGE)] // 80
async fn database_pool() { ... }

// You can also use raw u8 numbers!
#[service(priority = 60)] 
async fn important_worker() { ... }

#[service(priority = ServicePriority::DEFAULT)] // 50
async fn business_logic() { ... }

#[service(priority = ServicePriority::EXTERNAL)] // 0
async fn web_api() { ... }
```

### The Priority Value
Under the hood, `priority` is a simple **`u8`** value. You are not limited to the pre-defined constants! Feel free to use any number between `0` and `255` to fine-tune your startup waves.

## 2. Startup: High to Low

When the daemon starts, it group services into waves based on their priority.
1.  **Wave 100** starts first. The daemon waits for all services in this wave to reach a `Healthy` state (by calling `done()` or hitting a loop).
2.  **Wave 80** starts only after Wave 100 is stable.
3.  ...and so on, down to **Wave 0**.

## 3. Shutdown: Low to High

When you stop the system (Ctrl+C), the process reverses. We want to stop the "outer" layers first to prevent new requests from entering while we clean up.
1.  **Wave 0** is stopped first. The daemon signals these services and waits for them to exit.
2.  **Wave 50** is stopped next.
3.  ...finally, **Wave 100** (Logging/Metrics) is the last to go, ensuring we capture all logs from the shutdown process.

## 4. Why Waves?

*   **Dependency Safety**: Your business logic can safely assume the database is ready because it's in a higher priority wave.
*   **Log Integrity**: You'll never miss a "Shutdown Complete" log because the logging system is the last thing to stop.
*   **Predictable Lifecycle**: No more race conditions where components die in a random order.

---

[**<- Previous Step: Error Handling & Retries**](docs/guide/tutorial/error-handling.md) | [**Next Step: Unit Testing & Simulation ->**](docs/guide/tutorial/unit-testing.md)

# Under the Hood

You've learned how to use the framework. Now, let's take a look at the gears and pulleys that make the magic happen. Understanding these core components will help you debug complex issues and design highly efficient systems.

---

## 1. The Registry (The Blueprint)

The **Registry** is a collection of `ServiceEntry` structures. 
*   When you use `#[service]`, a static entry is generated and added to a magic "distributed slice" (powered by the `linkme` crate).
*   The Registry is **Lazy**. It doesn't start services; it just knows how to create them.

> [!TIP]
> You can filter the registry by **Tags**. This allows you to run "Core" services in one process and "Edge" services in another, even if they are all compiled into the same binary.

For a deep dive into how macros generate these entries, see [Macros Deep Dive](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/macros-deep-dive.md).

## 2. The Runner (The Engine)

The **Runner** is responsible for the actual `tokio::spawn` calls. 
*   It manages the **Restart Policy**.
*   It handles the **Handshake Protocol**. When a service starts, the Runner waits for the `Healthy` status before starting the next wave.
*   It monitors for **Panics**. If a service thread crashes, the Runner catches it, records the error, and schedules a restart.

Learn more about the internal orchestration logic in the [Internal Overview](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/internal-overview.md).

## 3. The Status Plane (The Observer)

The **Status Plane** is a thread-safe, shared map of every service's current `ServiceStatus`.
*   This is what the `state()` function queries.
*   **Lock Tracing (Macro Illusion)**: When you use `#[service]`, the macro automatically redirects standard `RwLock` and `Mutex` to our tracked versions. This allows the system to monitor lock contention and automatically notify the `Watch` triggers when state changes.

## 4. The Shelf (The Soul)

The **Shelf** is a simple key-value store tied to the lifetime of the `ServiceDaemon`. 
*   It uses `Any` for type-safe storage and retrieval.
*   Values on the shelf survive service restarts (Reloads/Crashes) but are cleared when the entire Daemon process stops.

Explore advanced [Lifecycle Management](https://github.com/loft-games/service-daemon-rs/blob/master/docs/architecture/lifecycle-management.md) to see how components collaborate.

---

[**<- Previous Step: Playing God: Simulator**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/playing-god.md) | [**Next Step: Tailor-Made Triggers ->**](https://github.com/loft-games/service-daemon-rs/blob/master/docs/guide/tutorial/tailor-made-triggers.md)

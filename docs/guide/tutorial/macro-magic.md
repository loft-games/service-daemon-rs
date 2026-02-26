# Macro Magic Unleashed

The `#[service]` and `#[trigger]` macros are the soul of the framework's "Invisible" design. But they are not just for basic registration—they are highly extensible.

---

## 1. The Power of Tags

We've mentioned tags before, but let's see why they are a "Power User" feature. Tags allow you to create **Application Profiles**.

```rust
#[service(tags = ["critical", "api"])]
async fn payment_gateway() { ... }

#[service(tags = ["worker", "cleanup"])]
async fn log_purger() { ... }
```

In your `main.rs`, you can choose which "personality" the process assumes:

```rust
// Only run the critical API services in this container
let reg = Registry::builder().with_tag("api").build();
ServiceDaemon::builder().with_registry(reg).build().run().await?;
```

## 2. Accessing Metadata via `ServiceEntry`

The macros generate a `ServiceEntry` struct at compile time. This struct is publicly accessible and contains metadata about your service:
*   `name`: The function name.
*   `priority`: The assigned priority level.
*   `tags`: The list of tags.

You can use this to build **Internal Discovery Systems** or **Health Check Dashboards** that automatically list all services in the binary without manual hardcoding.

## 3. Customizing Macro Outputs (Future-Proofing)

Because the project uses `linkme`, the registration happens at the binary level. If you need to add your own custom metadata (like "Department Name" or "Service Owner") to the services, you can contribute to the `ServiceEntry` struct in the framework or use the Tag system to encode this information (e.g., `tags = ["owner:billing"]`).

## 4. Why stick with the Macros?

You *could* build a `ServiceDescription` manually and pass it to the daemon. But by using the macros, you benefit from:
1.  **Compile-time Discovery**: No missing services due to typos.
2.  **Automatic DI Mapping**: The macro analyzes your function arguments and writes the injection code for you.
3.  **Unified Lifecycle**: All services get the same robust error handling and restart logic for free.

---

## Congratulations!

You've completed the Grand Tour. You've gone from a simple heartbeat to understanding the deep internals and extensibility of `service-daemon-rs`.

Now, go forth and build something reliable!

[**← Previous Step: Tailor-Made Triggers**](tailor-made-triggers.md) | [**Back to the Grand Tour →**](grand-tour.md)

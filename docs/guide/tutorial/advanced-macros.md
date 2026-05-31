# Advanced Macro Usage

> [!NOTE]
> This is advanced reference material, not part of the beginner quick-start path.

The `#[service]` and `#[trigger]` macros do more than register a function -- they accept attribute arguments that drive selection, scheduling, and dispatch. This page covers the ones beyond the basics.

---

## 1. Tags

Tags attach string labels to a service. The daemon can filter services by tag at startup, which is how you build **Application Profiles**.

```rust,ignore
#[service(tags = ["critical", "api"])]
async fn payment_gateway() { ... }

#[service(tags = ["worker", "cleanup"])]
async fn log_purger() { ... }
```

In your `main.rs`, you can choose which "personality" the process assumes:

```rust,ignore
// Only run the critical API services in this container
let reg = Registry::builder().with_tag("api").build();
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut daemon = ServiceDaemon::builder()
        .with_registry(reg)
        .build();

    daemon.run().await;
    daemon.wait().await?;
    
    Ok(())
}
```

## 2. Accessing Metadata via `ServiceEntry`

The macros generate a `ServiceEntry` struct at compile time. This struct is publicly accessible and contains metadata about your service:
*   `name`: The function name.
*   `priority`: The assigned priority level.
*   `tags`: The list of tags.

You can use this to build **Internal Discovery Systems** or **Health Check Dashboards** that automatically list all services in the binary without manual hardcoding.

## 3. Service Metadata Boundaries

Because the project uses `linkme`, the registration happens at the binary level. Use the tag system for service metadata such as ownership (e.g., `tags = ["owner:billing"]`). `ServiceEntry` does not carry per-service restart overrides, scheduling hints, or experimental scheduling fields; keep those concerns in explicitly designed APIs rather than overloading registry metadata.

## 4. Why stick with the Macros?

You *could* build a `ServiceDescription` manually and pass it to the daemon. But by using the macros, you benefit from:
1.  **Compile-time Discovery**: No missing services due to typos.
2.  **Automatic DI Mapping**: The macro analyzes your function arguments and writes the injection code for you.
3.  **Unified Lifecycle**: Every service goes through the same supervisor -- error handling, backoff, and restart policy are applied consistently, with no per-service boilerplate.

---

## More Information

For the full macro implementation model, see [Macro Expansion](../../architecture/macro-expansion.md). For normal user-facing usage, return to the [README](../../../README.md) documentation section.

[Back to README](../../../README.md)

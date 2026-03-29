# Reactive Triggers

In the previous chapter, we built a service that runs in a loop. But what if you want your code to run only when something happens? This is where **Triggers** come in.

A Trigger is a specialized service that sleeps until an event occurs.

---

## 1. The Queue Trigger

The most common trigger is the `Queue`. Imagine you have a background job queue, and you want a worker to process items as they arrive.

```rust,ignore
use service_daemon::prelude::*; // TT is here!

#[provider(Queue(Job))]
pub struct JobQueue;

#[trigger(Queue(JobQueue))]
async fn job_worker(job: Job) -> anyhow::Result<()> {
    tracing::info!("Processing job {}: {}", job.id, job.payload);
    // Business logic goes here...
    Ok(())
}
```

> [!TIP]
> You can have multiple workers for the same queue. All handlers subscribed to a `Queue` will receive every message (broadcast/fanout).

## 2. Triggering the Trigger (Chain Reactions)

Triggers are even more powerful when they talk to each other. A trigger can "fire" another trigger by publishing an event.

Let's say after processing a `Job`, we want to notify a cleanup service.

```rust,ignore
// A simple signal provider
#[provider(Notify)]
pub struct CleanupSignal;

#[trigger(Queue(JobQueue))]
async fn process_and_notify(
    job: Job, 
    signal: Arc<CleanupSignal> // The framework injects this automatically!
) -> anyhow::Result<()> {
    tracing::info!("Work done on {}", job.id);
    
    // Fire the signal directly. 
    // The framework handles the back-end orchestration.
    signal.notify();
    
    Ok(())
}

#[trigger(Notify(CleanupSignal))]
async fn cleanup_handler() -> anyhow::Result<()> {
    tracing::info!("Cleaning up temporary files...");
    Ok(())
}
```

## 3. Explicit Payloads with `#[payload]`

By default, the framework treats the first argument that is *not* an `Arc<T>` as the event payload. 

However, if your payload is also wrapped in an `Arc` (common for zero-copy job processing) or if you want to be explicit, use the **`#[payload]`** attribute:

```rust,ignore
#[trigger(Queue(JobQueue))]
async fn complex_worker(
    #[payload] job: Arc<ComplexJob>, // Explicitly marked as payload
    db: Arc<Database>               // This is a DI resource
) -> anyhow::Result<()> {
    tracing::info!("Working on job {}", job.id);
    Ok(())
}
```

## 4. Why use Triggers instead of Loops?

1.  **Efficiency**: Triggers consume zero CPU while waiting. 
2.  **Decoupling**: The service sending the data doesn't need to know who is listening.
3.  **Scalability**: You can add more cleanup handlers just by adding more `#[trigger(Notify(CleanupSignal))]` functions.
4.  **Resilience**: If a handler fails, the framework automatically retries it with exponential backoff!
5.  **Elastic Scaling**: For streaming templates like `Queue`, the framework dispatches handlers asynchronously and automatically scales concurrency based on pressure via the dedicated `ScalingPolicy`. Other templates dispatch serially with no scaling overhead. You can customize these limits globally-see [**Error Handling & Retries**](./error-handling.md#2-mastering-throughput-scaling-policy).

> [!TIP]
> **Advanced Reading**: For a complete list of built-in triggers and details on custom retry policies, refer to the [Reactive Triggers Guide](../triggers.md).

---

[**<- Previous Step: Hello, Heartbeat!**](./hello-heartbeat.md) | [**Next Step: State Management & Recovery ->**](./state-recovery.md)

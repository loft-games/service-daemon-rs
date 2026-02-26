# Reactive Triggers

In the previous chapter, we built a service that runs in a loop. But what if you want your code to run only when something happens? This is where **Triggers** come in.

A Trigger is a specialized service that sleeps until an event occurs.

---

## 1. The Queue Trigger

The most common trigger is the `Queue`. Imagine you have a background job queue, and you want a worker to process items as they arrive.

```rust
use service_daemon::prelude::*; // TT is here!

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub payload: String,
}

#[trigger(Queue(JobQueue))]
async fn job_worker(job: Job) -> anyhow::Result<()> {
    tracing::info!("Processing job {}: {}", job.id, job.payload);
    // Business logic goes here...
    Ok(())
}
```

> [!TIP]
> You can have multiple workers for the same queue. Use `LBQueue` (Load Balancing) to distribute jobs to only one worker at a time, or `Queue` (Broadcast) to send the same job to everyone.

## 2. Triggering the Trigger (Chain Reactions)

Triggers are even more powerful when they talk to each other. A trigger can "fire" another trigger by publishing an event.

Let's say after processing a `Job`, we want to notify a cleanup service.

```rust
// A simple signal provider
#[provider]
pub struct CleanupSignal;

#[trigger(Queue(JobQueue))]
async fn process_and_notify(job: Job) -> anyhow::Result<()> {
    tracing::info!("Work done on {}", job.id);
    
    // Notify the cleanup signal! 
    // We use the global `publish` function to send events.
    service_daemon::publish(CleanupSignal).await;
    
    Ok(())
}

#[trigger(Notify(CleanupSignal))]
async fn cleanup_handler() -> anyhow::Result<()> {
    tracing::info!("Cleaning up temporary files...");
    Ok(())
}
```

## 3. Why use Triggers instead of Loops?

1.  **Efficiency**: Triggers consume zero CPU while waiting. 
2.  **Decoupling**: The service sending the data doesn't need to know who is listening.
3.  **Scalability**: You can add more cleanup handlers just by adding more `#[trigger(Notify(CleanupSignal))]` functions.

---

[**← Previous Step: Hello, Heartbeat!**](hello-heartbeat.md) | [**Next Step: The Art of Recovery →**](art-of-recovery.md)

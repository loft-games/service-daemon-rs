# Incremental migration recipe

Port an existing Rust/Tokio codebase onto service-daemon-rs one piece at a time.
The daemon can host a single service first and absorb the rest later, so you never
need a big-bang rewrite.

## 1. Inventory what you have

Map each existing construct to its framework primitive:

| You currently have | Becomes |
| :--- | :--- |
| A `tokio::spawn`ed loop you supervise by hand | a `#[service]` |
| Dynamically-created worker instances with per-instance config | a `#[service]` template with `#[input] cfg: &Cfg` plus daemon-bound `ServiceHandle::create(cfg)` / `start(cfg)` |
| A shared client / pool / config behind a global or `OnceCell` | a `#[provider]` injected as `Arc<T>` |
| An `mpsc`/`broadcast` consumer task | a `#[trigger(Queue(..))]` |
| A timer / interval task | a `#[trigger(Cron(..))]` |
| A "reload when X changes" task | a `#[trigger(Watch(X))]` over managed state |
| Hand-rolled restart/backoff/shutdown plumbing | delete it — the supervisor owns it |

## 2. Port bottom-up

Dependencies must exist before the things that need them, so port in this order:

1. **Providers first.** Move shared-resource construction into `#[provider]`
   functions returning plain `T` (or `Result<T, ProviderError>` for fallible
   init). Everything downstream injects `Arc<T>`.
2. **Services next.** Wrap each long-running loop in `#[service]`. Keep the loop
   body; replace the spawn/supervise glue with the macro. Use the interruptible
   `service_daemon::sleep(d).await` (returns `false` on shutdown) instead of
   `tokio::time::sleep`, and `service_daemon::is_shutdown()` as the loop guard.
3. **On-demand services when needed.** If an old task is instantiated with
   runtime config, declare one `#[input] cfg: &Cfg` parameter, select the template
   by tag, expose a daemon-bound `ServiceHandle` from a provider, and create
   instances through that handle.
4. **Triggers last.** Convert event consumers, timers, and reload reactions into
   `#[trigger]`s so the framework drives them.
5. **Bootstrap.** Assemble `main()` with `ServiceDaemon::builder()`, then
   `daemon.run().await; daemon.wait().await?;`.

## 3. Readiness and the handshake

A service that needs an explicit "I'm ready" point calls `service_daemon::done()`
once initialization finishes — this advances it to `Healthy` so the next startup
wave may proceed. If you don't call it, the first `is_shutdown()` / `sleep()` does
an implicit handshake, which is fine for simple loops.

## 4. Ordering and recovery

- Give each service a `priority` (`u8`, default `50`; constants `EXTERNAL = 0`,
  `DEFAULT = 50`, `STORAGE = 80`, `SYSTEM = 100`). Startup runs high→low in waves;
  shutdown runs low→high. Put infrastructure high so it comes up first and goes
  down last.
- Tune supervision globally with `RestartPolicy` (restart limits, backoff,
  `provider_init_timeout`) via `with_restart_policy(..)`. Don't reimplement this
  in business code.

## 5. State that must survive a restart

In-memory fields are lost when a generation terminates. Move durable checkpoints
to the per-service **Shelf** (keyed, async):

```rust
let resumed: Option<Checkpoint> = service_daemon::unshelve("ckpt").await; // retrieve+remove
// ... work ...
service_daemon::shelve("ckpt", checkpoint.clone()).await;     // deposit under the key
```

For mutable state shared *across* services plus change notifications, use managed
state (`Arc<RwLock<T>>`) + a `Watch` trigger instead — see `sd-state-management`.

## 6. Verify one service at a time

Adopt incrementally: register just the first ported service (tag-filter the
`Registry` if needed), confirm it starts/stops cleanly under the daemon, then port
the next. This keeps each step small and reversible.

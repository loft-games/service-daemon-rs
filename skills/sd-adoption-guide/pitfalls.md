# Migration pitfalls

## Attempting a big-bang rewrite

The daemon can host one service while the rest of your app runs the old way. Port
the highest-value loop first, verify it under the supervisor, then absorb more.
Converting everything at once removes your ability to bisect what broke.

## Leaving supervision logic in business code

If you keep your hand-rolled restart/backoff/shutdown plumbing *and* adopt the
framework, you now have two supervisors fighting. Delete the manual plumbing and
let `RestartPolicy` + the supervisor own restart, backoff, and shutdown.

## Returning `Arc<T>` from a provider

A `#[provider]` returns plain `T` (or `Result<T, ProviderError>`); the framework
wraps it in `Arc` and hands `Arc<T>` to consumers. Returning `Arc<T>` yourself
double-wraps and breaks inference.

## Using `tokio::time::sleep` inside a service loop

`tokio::time::sleep` is not shutdown-aware, so a sleeping service won't notice a
shutdown until it wakes. Use `service_daemon::sleep(d).await`, which returns
`false` the moment shutdown arrives, and guard the loop with `is_shutdown()`.

## Forgetting `wait()` after `run()`

`run()` is non-blocking — it returns once services are up. If `main()` ends there,
the `daemon` is dropped and everything shuts straight back down. Always follow with
`daemon.wait().await?` (or another blocking call). See `sd-daemon-bootstrap`.

## Assuming struct fields survive a restart

When a service generation terminates (crash/restart/reload), its in-memory state is
gone. Anything that must survive belongs on the keyed Shelf (`shelve` / `unshelve`),
not in a struct field you hope sticks around.

## Priority inversion between a service and its dependency

Startup is descending priority. An edge listener at the default `50` is not
guaranteed to start after the database it needs if the database is also `50`. Give
infrastructure a higher priority (`STORAGE` / `SYSTEM`) so wave ordering brings it
up first and tears it down last.

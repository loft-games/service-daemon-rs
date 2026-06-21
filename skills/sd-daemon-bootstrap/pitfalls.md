# Daemon bootstrap pitfalls

## Calling `run()` but never `wait()`

`run()` is non-blocking — it spawns the services and returns. If `main()` ends
there, `daemon` is dropped and everything tears straight back down, so the
process appears to "start and immediately exit". Always follow `run().await`
with `wait().await?` (or some other call that blocks the task) to keep the
daemon alive.

## Dropping the `ServiceDaemon` while you still need it

The daemon owns the supervision tasks. Letting it go out of scope shuts the
system down. Keep it bound (e.g. `let mut daemon = ...;`) for the whole lifetime
of the process.

## A misspelled or missing tag silently selects nothing

Tag filtering does not error on unknown tags. `with_tag("wbe")` (typo) just
matches zero services, so the daemon comes up empty. If "nothing runs", check
the tag spelling against the `#[service(tags = [...])]` declarations first.

## Priority inversion between a service and its dependency

Startup is **descending** priority. If an edge listener (e.g. an HTTP server)
keeps the default `50` while the database it needs is also `50`, wave ordering
does not guarantee the DB is healthy first. Give infrastructure a higher
priority (`STORAGE = 80`, `SYSTEM = 100`) so it starts before — and stops
after — its dependents.

## Reaching for `run_for_duration` in a production binary

`run_for_duration` is gated behind `#[cfg(feature = "simulation")]`. It will not
compile in a normal build and is not meant for production — it auto-shuts-down
after the duration. Use it only in tests; use `run()` + `wait()` everywhere else.

## Forgetting the async runtime

`run`/`wait` are `async`. `main()` must be on a runtime (`#[tokio::main]`), or
the futures never get polled and the daemon never starts.

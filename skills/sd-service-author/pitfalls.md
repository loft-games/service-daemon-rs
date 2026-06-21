# `#[service]` pitfalls

## Using `tokio::time::sleep` in the loop

`tokio::time::sleep` ignores shutdown — the service finishes the full delay before
noticing it should stop. Use `service_daemon::sleep(dur).await` and break when it
returns `false`.

## Never reaching `Healthy`

If a service with heavy initialization never calls `done()` and never touches a
lifecycle helper, it can leave dependents in later waves waiting until
`wave_spawn_timeout`. Call `service_daemon::done()` once init is complete.

## Misclassifying failure

- Returning `ServiceError::Fatal` for a transient error permanently terminates a
  service that could have recovered. Reserve `Fatal` for unrecoverable conditions.
- Returning an ordinary `Err`/panicking for an unrecoverable condition causes an
  endless restart-with-backoff loop. Use `Fatal` to stop cleanly.

## Treating `Ok(())` as "done forever"

A clean `Ok(())` return starts a fresh generation immediately. If a service should
run once and stop, that is not what `Ok(())` means — model completion explicitly
(e.g. wait on shutdown) rather than returning early.

## Swallowing resource errors with `?`

On resource-acquisition paths, `?` can hide what the supervisor observes. Match
explicitly and return a structured `ServiceError` (e.g.
`ServiceError::runtime_io(..)`) so the failure is classified correctly.

## Sync service without annotation

A synchronous `#[service] fn` warns at runtime. Either make it `async`, or annotate
`#[allow(sync_handler)]` when it is intentionally synchronous and does no I/O.

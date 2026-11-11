# `#[service]` pitfalls

## Treating HighPriority as a latency guarantee

HighPriority requires the opt-in `high-priority` feature. It permits automatic
resource intervention and reload, not a hard latency SLA or lossless business
restart. Do not add artificial sleeps or manual round markers to satisfy policy;
the initial controller only acts where real ServiceSleep evidence exists.
Synthetic calibration is not an application SLA. External async wait outside
the measured sleep is not evidence of low-benefit convergence; do not add sleeps
to manufacture such evidence. Maintainer calibration commands belong in
`docs/development/release-validation.md`, not application setup instructions.

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

A clean `Ok(())` return without a shutdown or reload signal is unexpected
termination for a long-running service. The daemon records `NormalExit` and
starts a fresh generation through `RestartPolicy` normal-exit backoff. If a
service should run once and stop, model completion explicitly outside the service
lifecycle rather than returning early.

## Swallowing resource errors with `?`

On resource-acquisition paths, `?` can hide what the supervisor observes. Match
explicitly and return a structured `ServiceError` (e.g.
`ServiceError::runtime_io(..)`) so the failure is classified correctly.

## Sync service without annotation

A synchronous `#[service] fn` warns at runtime. Either make it `async`, or annotate
`#[allow(sync_handler)]` when it is intentionally synchronous and does no I/O.

## Bare service parameters without `#[input]`

Services do not accept trigger-style payload parameters. A non-`Arc` service
parameter must be the single on-demand startup input and must be marked
`#[input]`. Otherwise wrap dependencies as `Arc<T>`, `Arc<RwLock<T>>`, or
`Arc<Mutex<T>>`.

## Invalid `#[input]` shape

`#[input]` is only for service templates and must look like
`#[input] cfg: &Config`: immutable reference, lifetime elision, no `Arc`, no
`&mut`, and only one input parameter per service. Wrap multiple values in one
owned config struct and pass that to `ServiceHandle::create(input)` or
`ServiceHandle::start(input)`.

## Expecting service templates to auto-start

A service with `#[input]` is selected by the registry but starts with zero
instances. Resolve a daemon-bound `ServiceHandle` and call `create(input)` or
`start(input)`; otherwise the template definition is available but no worker body
runs.

## Passing input to a non-template service handle

Services without `#[input]` already auto-start one instance when selected. If you
manually create another instance through a `ServiceHandle`, pass `()` only.
Passing any other value is rejected with "does not declare #[input] but received
input type".

# Simulation testing pitfalls

## Forgetting to enable the `simulation` feature

`MockContext`, `SimulationHandle`, and `SimulationHandle::run_for_duration` only
exist under `features = ["simulation"]`. Without it the test won't compile (the
symbols are absent). Enable the feature on the `service-daemon` dependency in the
test crate.

## Enumerating runtime handles before services have spawned

The status plane is populated as the runner spawns services. Calling
`simulation.service_instances()` immediately after `run_for_duration` starts may
return an empty or partial list. For mid-flight work, let services spawn first (a
short `tokio::time::sleep`) before enumerating handles. Runtime mutation/read APIs
take `&ServiceInstanceHandle`, not a bare `ServiceInstanceId`.

## Expecting `run_for_duration` to block forever

`run_for_duration` returns after the duration elapses (auto-shutdown). It is a
bounded, deterministic driver — not `run()`/`wait()`. If your assertion needs the
service to have done its work, size the duration to cover it, or use the spawn +
mid-flight pattern and `await` the task.

## Asserting through a held lock instead of the read API

Don't reach into the resources and hold a `DashMap` guard across an `.await`. Use
the lock-free readers — `get_shelf` / `get_status` / `has_shelf` / `shelf_keys` —
which acquire and release internally and return owned values, so they are safe
around await points.

## Pre-filling with the wrong `ServiceInstanceId`

`with_shelf(service_instance_id, ...)` silently targets whatever ID you pass; a
wrong ID puts the data in a bucket the service never reads, and the test fails
with an empty read rather than an obvious error. Keep one service under test and
derive its instance ID unambiguously.

## Type mismatch between `set_shelf` and `unshelve`

The Shelf stores `Box<dyn Any>` keyed by type. If the service `unshelve::<A>()`s
but the test `set_shelf::<B>()`d, the downcast fails and the read yields `None`.
Match the concrete type (and key) exactly on both sides.

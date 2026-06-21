# Example authoring pitfalls

## Forgetting to add the crate to the workspace `members`

The root `Cargo.toml` `members` list is explicit, not a glob. A new
`examples/<dir>` that isn't listed there is silently excluded from
`cargo build/test --workspace` and from CI — it looks fine locally if you build it
directly, then "doesn't exist" to everyone else. Add the path when you create it.

## Porting real business logic into an example

Examples teach the framework, not a product. Importing real domain models, business
rules, or service code obscures the mechanic and rots as the product changes. Keep
flows generic (counters, `"Broadcast #{n}"`) and reproduce only the framework shape.

## Swallowing framework errors with `?` on resource acquisition

A bare `?` on `listener.get()` / `local_addr()` flattens the failure into an opaque
`anyhow` error, hiding the classification the supervisor acts on. On resource-
acquisition paths use the explicit form — `match ... ServiceError::runtime_io(..)`
or `.map_err(|e| ServiceError::runtime_io(..))?` — so framework semantics stay
visible. `?` is fine elsewhere where the error meaning is uninteresting.

## Returning `Arc<T>` from a provider in an example

Same rule as production: `#[provider]` returns plain `T`; the framework wraps it.
An example that returns `Arc<T>` teaches the wrong shape and breaks inference.

## Module docs that describe the domain instead of the mechanic

`//!` docs should say "demonstrates Queue-trigger fan-out from a source service",
not "an order-processing pipeline". The reader is here to learn the framework, so
the doc must name the framework mechanic the file exercises.

## Wrong crate metadata

Drop the `publish = false` and an example can leak to a publish run; use a name
without the `example-` prefix and tooling/CI conventions break. Keep
`name = "example-<dir>"`, `version = "0.1.0"`, `edition = "2024"`, `publish = false`.

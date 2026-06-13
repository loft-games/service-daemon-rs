# Macro development pitfalls

## Changing an entry struct without updating every emitter

`ServiceEntry` is emitted by **both** `#[service]` and `#[trigger]`. Adding,
removing, or reordering a field means updating both `service/codegen.rs` and
`trigger/codegen.rs` (and `provider/struct_gen.rs` for `ProviderEntry`). Miss one
and the generated code stops compiling — or worse, compiles with a wrong field
order. The struct in `service-daemon/src/models/service.rs` and the codegen are a
single contract; change them together.

## Dropping `#[allow(unsafe_code)]` on the distributed slices

`linkme`'s `#[distributed_slice]` expands to `#[link_section]`, which Rust edition
2024 treats as `unsafe`. The slices carry `#[allow(unsafe_code)]` for exactly this
reason. Removing it (or the crate-level allowance) breaks the build under the 2024
edition. Keep the allowance scoped to the slice declarations.

## Treating linkme support as one platform contract

`linkme` may support an OS family while individual linker/object-format paths
behave differently. Windows GNU has a known section-GC workaround path; Linux
GNU, Linux musl, Windows MSVC, and macOS are positive smoke paths. Keep runtime
registry assertions and binary registry section/symbol checks in CI, and keep
those expectations in `docs/development/release-validation.md` separate. Do not
expand this into every target triple.

## Editing the parser and codegen as one blob

The modules deliberately split parsing (`parser.rs`) from emission (`codegen.rs` /
`struct_gen.rs` / `templates.rs`). Folding attribute parsing into the token-emitting
code makes both harder to test and review. Add new attribute handling in the
parser; add new generated shapes in codegen.

## Forgetting to refresh trybuild `.stderr` snapshots

A change to a diagnostic message desyncs the `tests/fail/*.stderr` snapshot, failing
`compile_fail`. Regenerate the snapshot (trybuild supports this) and **review the
diff** — a blindly-accepted snapshot can hide a regression where the macro now
rejects the wrong thing or with a worse message.

## Adding a rejected pattern without a fixture

If you make the macro reject a new misuse, add a `tests/fail/NN_*.rs` fixture plus
its `.stderr`. Otherwise the new error is untested and a later refactor can silently
stop rejecting it. The `tests/fail/` directory is the spec for what the macros must
refuse.

## Reading expansion from the wrong crate

`cargo expand` on a tiny example may not exercise triggers, providers, and watchers
together. Use `cargo expand -p example-complete`, which covers all three macro
families, so you see the full emitted surface rather than a partial one.

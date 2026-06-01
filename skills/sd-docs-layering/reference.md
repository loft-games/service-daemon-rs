# Documentation layer map

The current `docs/` tree, by layer. Use it to find where an existing topic lives
and where a new page belongs.

## `docs/guide/tutorial/` — beginner, sequential

The guided learning path. `quick-start.md` is the chapter index; the rest are
ordered lessons. A new beginner lesson goes here **and** is linked from
`quick-start.md`.

- `quick-start.md` — chapter index / front door (also linked from the repo `README.md`).
- `first-service.md` — your first `#[service]`.
- `reactive-triggers.md` — events, queues, chained handlers.
- `state-recovery.md` — persistence across restarts (the Shelf).
- `priority-orchestration.md` — priority waves + runtime scheduling.
- `error-handling.md` — fallible flows.
- `custom-providers.md` — provider authoring on the learning path.
- `custom-trigger-hosts.md`, `trigger-interceptors.md`, `advanced-macros.md`,
  `unit-testing.md` — later, more advanced chapters.

## `docs/guide/` — topical user reference

Standalone deep-dives for a user who already knows the basics:

- `triggers.md`, `provider-best-practices.md`, `resilience.md`,
  `state-management.md`, `interceptor-middleware.md`, `diagnostics.md`,
  `testing-troubleshooting.md`, `faq.md`.

A new "how do I do X with the framework" page that isn't a sequential lesson goes
here, not in `tutorial/`.

## `docs/development/` — contributor/maintainer

- `extending-framework.md` — adding new trigger hosts, extending macro behavior,
  the seams a maintainer works against.

## `docs/architecture/` — internal mechanics

Not API usage — how the framework works inside:

- `internal-overview.md` — the big picture (registries, DI resolution, status plane).
- `lifecycle-management.md` — wave orchestration, supervisor FSM, provider-init
  error semantics (`ProviderError` Fatal/Retryable, `RestartPolicy`).
- `macro-expansion.md` — what `#[service]`/`#[trigger]`/`#[provider]` generate.
- `causal-tracing.md`, `performance-benchmarks.md`.

## `docs/CONTRIBUTING.md` — workflow

Build/test/lint commands, Conventional Commits, PR expectations. Not a layer of the
docs tree but the entry point for *how* to contribute docs and code.

## Where does a new page go? (quick map)

| The page is… | Layer |
| :--- | :--- |
| A next step in the beginner journey | `docs/guide/tutorial/` (+ link in `quick-start.md`) |
| A focused user how-to on one capability | `docs/guide/<topic>.md` |
| Guidance for someone extending the framework | `docs/development/` |
| An explanation of an internal mechanism | `docs/architecture/` |
| A process/workflow rule | `docs/CONTRIBUTING.md` |

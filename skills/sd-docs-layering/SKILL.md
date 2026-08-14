---
name: sd-docs-layering
description: "[dev] Place documentation in the right layer of the service-daemon-rs docs/ tree. Use when adding or reviewing docs and deciding whether content belongs in the beginner tutorial, the topical guide reference, the development (maintainer) docs, or the internal architecture docs."
---

# Documentation layering in service-daemon-rs

`docs/` is layered **by audience**. Putting content in the wrong layer is the most
common docs mistake: advanced/strict APIs leaking into the beginner path, or
internal mechanics surfacing in user-facing guides. Pick the layer by *who reads
it and why*.

| Layer | Path | Audience & purpose |
| :--- | :--- | :--- |
| Tutorial | `docs/guide/tutorial/` | **Beginners**, sequential. A guided path from first service onward. Keep the learning curve smooth — no strict/advanced asides. |
| Guide reference | `docs/guide/` | **Users** who already know the basics and want one topic in depth (triggers, resilience, state, diagnostics). |
| Development | `docs/development/` | **Contributors/maintainers** extending the framework itself. |
| Architecture | `docs/architecture/` | **Maintainers** — internal mechanics, not API usage. The "how it works inside". |

## The decision

Ask, in order:

1. *Is the reader learning the framework for the first time, step by step?* →
   `docs/guide/tutorial/`. New chapters slot into the `quick-start.md` chapter list.
2. *Is the reader a user looking up one capability they already know exists?* →
   `docs/guide/<topic>.md`.
3. *Is the reader extending the framework or maintaining release validation
   checks?* → `docs/development/`.
   - Extension mechanics go in `docs/development/extending-framework.md`.
   - Feature matrices, dependency baselines, linkme platform smoke coverage,
     example layers, and release checklists go in
     `docs/development/release-validation.md`.
4. *Is this internal mechanism explanation, not something a user calls?* →
   `docs/architecture/`.

`AGENTS.md` and `TODO.md` are internal working documents by default in this repo:
use them to guide current work, but do not migrate their current-only blockers,
handoff notes, or planning details into public docs, changelog entries, PR text,
or commit messages unless the user explicitly says that boundary is overridden.

## Keep the beginner path clean

The tutorial is a curated curve. Strict, advanced, or rarely-needed APIs do **not**
belong there — they go in the relevant `docs/guide/` reference page, with at most a
forward pointer from the tutorial. This is a deliberate project rule: protect the
smooth first-run experience.

## Companions

- `reference.md` — the full file map of every doc, by layer, and where a new page
  for a given subject goes.
- `pitfalls.md` — the layering traps (advanced API in the tutorial, user-facing
  vs internal confusion, orphaned pages).

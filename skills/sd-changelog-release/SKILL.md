---
name: sd-changelog-release
description: "[dev] Maintain CHANGELOG.md and cut releases for service-daemon-rs. Use when recording a change in the changelog, deciding the next version, or tagging a release — including knowing whether a version is already released so new work lands under [Unreleased]."
---

# Changelog & release discipline for service-daemon-rs

`CHANGELOG.md` follows **[Keep a Changelog 1.1.0]** and the project adheres to
**[Semantic Versioning 2.0.0]**. The project is in pre-1.0 alpha: versions look
like `0.1.0-alpha.N` and tags like `v0.1.0-alpha.N`.

## The one rule that prevents mistakes

**New work always lands under `## [Unreleased]`.** Released sections are dated and
tagged — they are history and must not be edited. Before you place a change under a
versioned heading, confirm whether that version is already released:

```bash
git tag | sort -V   # if v0.1.0-alpha.4 exists, that section is frozen
```

If the version is tagged, it shipped — your change belongs under `[Unreleased]`,
not in the released section.

`AGENTS.md` and `TODO.md` are internal working documents in this repository by
default. Do not quote or summarize their current-only planning details in
`CHANGELOG.md`, release notes, PR text, or public docs unless the user explicitly
asks to publish that internal context.

## Recording a change

Under `## [Unreleased]`, add a bullet to the right `###` subsection:

| Subsection | For |
| :--- | :--- |
| `Added` | new features / capabilities |
| `Changed` | changes to existing behavior |
| `Deprecated` | soon-to-be-removed features |
| `Removed` | removed features |
| `Fixed` | bug fixes |
| `Security` | vulnerability fixes |

Commits follow Conventional Commits, which map cleanly: `feat:` → **Added**,
`fix:` → **Fixed**, `refactor:`/`perf:` → **Changed**, a breaking change → a
version bump plus a **Changed**/**Removed** note.

Add a changelog entry when release-validation work changes visible behavior or
maintainer release gates. Record diagnostics output contract changes, file
logging failure semantics, linkme platform smoke coverage, and new release
validation documentation under `[Unreleased]`.

## Cutting a release

1. Decide the version from the accumulated `[Unreleased]` entries (SemVer; pre-1.0
   alpha increments the `alpha.N` suffix).
2. Rename `## [Unreleased]` to `## [<version>] - <YYYY-MM-DD>` and open a fresh
   empty `## [Unreleased]` above it.
3. Tag: `git tag v<version>` (e.g. `v0.1.0-alpha.5`).

## Companions

- `reference.md` — the exact file structure, current version history, the
  Conventional-Commits → section mapping, release checklist, and
  release-validation entry rules.
- `pitfalls.md` — the traps (editing a frozen release, wrong section, version/tag
  mismatch, dating before tagging).

[Keep a Changelog 1.1.0]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning 2.0.0]: https://semver.org/spec/v2.0.0.html

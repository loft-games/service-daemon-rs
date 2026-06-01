# Changelog & release reference

## 1. File shape (`CHANGELOG.md`)

The header states the format and versioning scheme, then sections run newest-first:

```markdown
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
### Added
- ...
### Changed
- ...
### Fixed
- ...

## [0.1.0-alpha.4] - 2026-05-07
### Added
- ...
```

`[Unreleased]` is always present and always on top. Each released heading is
`## [<version>] - <YYYY-MM-DD>`.

## 2. Version history (as released)

| Version | Date | Tag |
| :--- | :--- | :--- |
| `0.1.0-alpha.4` | 2026-05-07 | `v0.1.0-alpha.4` |
| `0.1.0-alpha.3` | 2026-03-29 | `v0.1.0-alpha.3` |
| `0.1.0-alpha.2` | 2026-03-14 | `v0.1.0-alpha.2` |
| `0.1.0-alpha.1` | 2026-03-04 | `v0.1.0-alpha.1` |

Confirm the current frontier with `git tag | sort -V` before assuming what is
released — the table above is a snapshot and the latest tag is authoritative.

## 3. Section subsections (Keep a Changelog order)

`Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. Only include the
ones that have entries. Keep order consistent within a release.

## 4. Conventional Commits → changelog mapping

| Commit type | Changelog subsection |
| :--- | :--- |
| `feat:` | Added |
| `fix:` | Fixed |
| `refactor:`, `perf:` | Changed (if user-visible) |
| `docs:`, `test:`, `chore:`, `ci:` | usually **no** changelog entry |
| `feat!:` / `BREAKING CHANGE:` | Changed/Removed **and** a version bump |

Not every commit earns a changelog line — internal-only changes (docs, tests,
chores) typically don't appear.

## 5. SemVer in pre-1.0 alpha

While `0.y.z`, the public API is not yet stable. This project encodes its
iterations as `0.1.0-alpha.N`; cutting the next release increments `N`. A move to
`0.1.0` (no pre-release) or `0.2.0` is a deliberate milestone decision, not an
automatic step.

## 6. Release checklist

1. `git tag | sort -V` — know the current frontier.
2. Review `[Unreleased]`; pick the next version per SemVer.
3. Rename `[Unreleased]` → `[<version>] - <today>`; add a fresh empty
   `[Unreleased]` above it.
4. Commit (`chore(release): v<version>` or similar Conventional Commit).
5. `git tag v<version>`.

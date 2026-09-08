# Changelog & release pitfalls

## Editing an already-released (tagged) section

A `## [<version>] - <date>` heading whose tag exists in `git tag` is frozen
history. Adding or rewriting bullets there silently rewrites the past and desyncs
the changelog from the tagged source. New work goes under `## [Unreleased]`. When
unsure, run `git tag | sort -V` first — that is the release boundary.

## Putting a change in the wrong subsection

First verify the previous released tag. A bug introduced and corrected within
the unreleased cycle is not a fix to the released product. Fold it into the final
feature description when relevant; do not list review fixes or unreleased API
redesigns as separate `Fixed` or `Changed` entries merely because their commits
have those prefixes.

A behavior change filed under `Added`, or a new feature filed under `Fixed`,
misleads readers scanning for breaking/behavioral changes. Map from intent: new
capability → `Added`, changed behavior → `Changed`, bug fix → `Fixed`,
vulnerability → `Security`.

## Version / tag mismatch

The version in the changelog heading and the git tag must agree. `## [0.1.0-alpha.5]`
with a tag `v0.1.0-alpha.6` (or a missing tag) breaks the link between the
changelog and the released artifact. Tag exactly `v<version>`.

## Dating or tagging before the release is final

Renaming `[Unreleased]` to a dated heading, or tagging, before the release content
is settled means a late change either edits a now-"released" section (see above) or
forces an awkward re-tag. Freeze the section and tag as the **last** steps.

## Logging internal-only commits as changelog entries

`docs:`, `test:`, `chore:`, `ci:` commits usually have no user-visible effect and
should not clutter the changelog. The changelog is the user's view of what changed
for them, not a commit log — `git log` already is the commit log.

## Forgetting to re-open `[Unreleased]` after a release

After renaming `[Unreleased]` to the dated version, add a fresh empty
`## [Unreleased]` on top. Otherwise the next contributor has nowhere correct to
record their change and may reopen the just-released section.

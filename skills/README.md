# service-daemon-rs Agent Skills

This directory ships [Agent Skills](https://docs.claude.com/en/docs/agents-and-tools/agent-skills/overview)
for `service-daemon-rs`. The repository acts as a **skills install provider**: each
skill is a self-contained `<name>/SKILL.md` directory. Install the ones you want
into your own agent tool; this repo is not coupled to any specific tool.

Each skill is a directory using **progressive disclosure**: a `SKILL.md` entry
point plus companion `reference.md` / `pitfalls.md` (and `examples/` where useful).
When you install a skill, copy the **whole directory**, not just `SKILL.md`.

User-facing skills are written to stand on their own — they do **not** require
cloning this repository. Maintainer-facing (`dev`) skills reference in-repo paths
because they are about working *inside* this repository.

## Catalog

User-facing — using the framework:

| Skill | Purpose |
| :--- | :--- |
| `sd-adoption-guide` | Whether and how to migrate an existing project onto the framework (journey front door) |
| `sd-service-author` | Writing `#[service]`: loop shape, readiness `done()`, interruptible `sleep`, restart, and `#[input]` service templates |
| `sd-provider-author` | Writing `#[provider]`: lazy/eager and `ProviderError::Fatal` vs `Retryable` |
| `sd-trigger-author` | Writing `#[trigger]`: host families `Queue` / `Cron` / `Signal` / `Watch` |
| `sd-state-management` | Managed state `Arc<RwLock<T>>`, `Watch` notifications, the keyed Shelf |
| `sd-daemon-bootstrap` | Assembling `main()`: builder, tag-filtered `Registry`, `run()` vs `wait()`, priority waves, and selected service-template behavior |
| `sd-simulation-testing` | Deterministic tests with `MockContext` / `SimulationHandle` |

Maintainer-facing — working on this repository:

| Skill | Purpose |
| :--- | :--- |
| `sd-example-authoring` | Conventions for adding `examples/*` crates |
| `sd-docs-layering` | Which `docs/` layer a new page belongs in |
| `sd-changelog-release` | Maintaining `CHANGELOG.md` and cutting releases |
| `sd-macro-development` | Working on the `service-daemon-macro` proc-macro crate |

> Skill maintenance notes are tracked with the repository's internal planning files.

## Installing (your choice)

Each skill is a self-contained directory. Place the ones you need into your tool's
skill discovery path:

- **Copy**: `cp -r skills/sd-provider-author <your-tool-skills-dir>/`
- **Symlink**: `ln -s "$(pwd)/skills/sd-provider-author" <your-tool-skills-dir>/sd-provider-author`

Common discovery paths:

- Claude Code: project-level `.claude/skills/`, or user-level `~/.claude/skills/`
- Some tools: `.agent/skills/`

> On Windows, symlinks require `git config core.symlinks true` plus Developer Mode
> or administrator privileges. When in doubt, copying is the safest option.

After installing, keep the one-level layout `<your-tool-skills-dir>/sd-provider-author/SKILL.md`.

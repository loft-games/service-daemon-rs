# service-daemon-rs Contributing Guide

[![test](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)
[![License](https://img.shields.io/github/license/loft-games/service-daemon-rs)](LICENSE)

Thank you for your interest in contributing to `service-daemon-rs`! This project is maintained by volunteers, and we welcome your help.

## Development Environment Setup

To develop the core framework or macros, you need:
- **Rust**: Latest stable version.
- **Tokio**: This framework is built entirely on the `tokio` runtime.
- **Cargo Expand**: Used for debugging macro expansion. Install via `cargo install cargo-expand`.

### Standard Workflow

1. **Modify Core/Macro**: Make changes in `service-daemon` or `service-daemon-macro`.
2. **Run Tests**: Use `cargo test --workspace` to ensure no regressions.
3. **Verify Expansion**: Use `cargo expand -p example-complete` to see how changes affect user code.

4. **Linting**: Follow the suggestions of `cargo clippy --workspace -- -D warnings`.

## Found a BUG?

1. Search the [GitHub Issues](https://github.com/loft-games/service-daemon-rs/issues) page to ensure no one has encountered it before.
2. If the issue is new, please [create a new Issue](https://github.com/loft-games/service-daemon-rs/issues/new) and describe the problem and reproduction steps in detail.
3. If possible, attach your code examples, system environment, and error logs to help us reproduce the issue.

## Proposing New Features or Changes?

1. Propose your suggested changes or feature requests in [Issues](https://github.com/loft-games/service-daemon-rs/issues) before starting to write code.
2. we aim to minimize breaking changes, so major architectural adjustments should be accepted only after thorough discussion.
3. If you're interested in extending the framework, please refer to the [Extending Framework Guide](docs/development/extending-framework.md).

## Submitting Changes

1. Open a new PR on GitHub.
2. Ensure the PR description clearly explains the problem and your solution, and link the corresponding Issue number if applicable.
3. Before submitting, ensure your commits follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).

`service-daemon-rs` is now a volunteer-maintained project, and we welcome and encourage you to join us.

Thank you!

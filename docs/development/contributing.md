# Contributing to service-daemon-rs

Thank you for your interest in contributing! This project is maintained by volunteers and we welcome your help.

## Development Environment Setup

To work on the core framework or macros:
- **Rust**: Latest stable version.
- **Tokio**: The framework is built exclusively on the `tokio` runtime.
- **Cargo Expand**: Useful for debugging macro expansion. Install via `cargo install cargo-expand`.

### Standard Workflow
1. **Modify Core/Macros**: Make changes in `service-daemon` or `service-daemon-macro`.
2. **Run Tests**: Use `cargo test --workspace` to ensure no regressions.
3. **Verify Expansion**: Use `cargo expand -p service-daemon-demo` to see how changes affect user code.

## Reporting Bugs
1. Search GitHub [Issues](https://github.com/loft-games/service-daemon-rs/issues) to ensure the bug hasn't been reported.
2. [Create a new issue](https://github.com/loft-games/service-daemon-rs/issues/new) with a detailed description and reproduction steps.

## Submitting Changes
1. Open a Pull Request (PR).
2. Ensure the PR description clearly explains the problem and solution.
3. Follow [Conventional Commits](https://www.conventionalcommits.org/).

[Back to README](../../README.md)

# service-daemon-rs 贡献指南

[![test](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/loft-games/service-daemon-rs/actions/workflows/rust.yml)
[![License](https://img.shields.io/github/license/loft-games/service-daemon-rs)](LICENSE)

感谢您对贡献 service-daemon-rs 的兴趣！本项目由志愿者维护，我们欢迎您的帮助。

## 开发环境搭建

要开发核心框架或宏，您需要：
- **Rust**: 最新稳定版本。
- **Tokio**: 本框架完全基于 `tokio` 运行时构建。
- **Cargo Expand**: 用于调试宏展开。通过 `cargo install cargo-expand` 安装。

### 标准工作流
1. **修改核心/宏**: 在 `service-daemon` 或 `service-daemon-macro` 中进行更改。
2. **运行测试**: 使用 `cargo test --workspace` 确保没有回归。
3. **验证展开**: 使用 `cargo expand -p example-complete` 查看更改如何影响用户代码。
4. **代码检查**: 遵循 `cargo clippy --workspace -- -D warnings` 的建议。

## 您遇到了一个 BUG?

1. 在 GitHub 的 [Issue](https://github.com/loft-games/service-daemon-rs/issues) 页面中搜索相关的问题，确保之前从没有人遇到过它。
2. 如果你确保这个问题从没有人提出过，请[创建一个新 Issue](https://github.com/loft-games/service-daemon-rs/issues/new) 并在其中详细描述你遇到的问题和复现的方法。
3. 如果可以，最好能附加上你使用的代码示例、系统环境以及错误日志，以帮助我们复现该问题。

## 您打算新增功能还是调整现有功能?

1. 在 [Issues](https://github.com/loft-games/service-daemon-rs/issues) 中提出您的更改建议或功能请求，然后开始编写代码。
2. 我们希望尽可能减少破坏性更新，因此重大的架构更改应当在充分讨论后才可能被接受。
3. 如果您对扩展框架感兴趣，请参阅[扩展框架指南](docs/development/extending-framework.md)。

## 提交更改

1. 在 GitHub 上打开一个新的 PR。
2. 确保 PR 描述清晰地解释了问题和您的解决方案，如果有对应的 Issue 请附上其编号。
3. 在提交之前，请确保您的提交遵循[约定式提交](https://www.conventionalcommits.org/zh-hans/v1.0.0-beta.4/)。

service-daemon-rs 现在是由志愿者维护的项目，我们欢迎且鼓励您加入我们。

谢谢 :heart: :heart: :heart:

Loft Games Teams

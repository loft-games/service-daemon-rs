# service-daemon-rs 开发文档

欢迎您的参与 :heart: :heart: :heart:，本文将帮助您完成 `service-daemon-rs` 的开发前置工作。

## 初始环境

- **操作系统**: 推荐使用 Linux 或 macOS。
- **编辑器**: 推荐使用 [Visual Studio Code](https://code.visualstudio.com/) 并安装 [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer) 插件。
- **Rust 工具链**: 需要安装 Rustup。请访问 [rustup.rs](https://rustup.rs/) 进行安装。推荐使用最新的稳定版（Stable channel）并开启 `edition = "2024"` 支持。

### 依赖项

除了 Rust 工具链，您可能还需要安装一些基础开发工具：

#### RedHat 系 (Fedora/CentOS)
```bash
sudo dnf groupinstall "Development Tools"
```

#### Debian 系 (Ubuntu/Debian)
```bash
sudo apt-get update
sudo apt-get install build-essential
```

#### ArchLinux 系
```bash
sudo pacman -S base-devel
```

## 开发与测试

1. **克隆仓库**:
   ```bash
   git clone https://github.com/MemoryShadow/service-daemon-rs.git
   cd service-daemon-rs
   ```

2. **编译并检查代码**:
   ```bash
   cargo check
   ```

3. **运行测试**:
   ```bash
   cargo test
   ```

4. **运行示例程序**:
   ```bash
   cargo run
   ```

## 代码规范

- 使用 `cargo fmt` 格式化代码。
- 使用 `cargo clippy` 进行代码静态分析。
- 遵循[约定式提交](https://www.conventionalcommits.org/zh-hans/v1.0.0-beta.4/)。

service-daemon-rs 现在是由志愿者维护的项目，我们欢迎且鼓励您加入我们。

谢谢 :heart: :heart: :heart:

MemoryShadow
# 贡献指南

感谢你对掠影（Lueying）输入法项目的关注。掠影基于 Windows TSF 与 Rust 实现，核心算法保持跨平台纯粹，平台相关实现集中在 TSF 与服务端边界。

在开始贡献代码前，请先通读[系统架构文档](docs/architecture.md)与[扩展开发概览](docs/extensions/README.md)。

> [!NOTE]
> 仓库根目录下的 `AGENTS.md` 专供自动化 Coding Agent 使用；人类贡献者请依照本文档准备本地环境与执行自测。

## 开发环境准备

- **操作系统**：Windows 10（1809+）或 Windows 11（x64）。
- **Rust 工具链**：以项目根目录的 `rust-toolchain.toml` 为准，需包含 `rustfmt` 与 `clippy` 组件。
- **打包工具**：构建 Windows 安装包需安装 Inno Setup 6.7.3。
- **Linux 支持**：Linux 环境可直接编译引擎、协议、扩展及无头渲染 E2E 测试，需预装基础开发库：`libfreetype6-dev`、`libfontconfig1-dev`。

### 外部词典与模型资源准备

词典与模型权重不随 Git 仓库提交，开发时可通过内置脚本拉取并构建：

```sh
# 抓取白霜词典、语法搭配与重排模型
bash tools/fetch_frost.sh data/frost
bash tools/fetch_gram.sh data
bash tools/fetch_rerank.sh data/rerank
bash tools/fetch_ppocr.sh data/ppocr

# 物化生成二进制内存映射主词典
cargo run -p xtask --release --locked -- dict --frost data/frost --out data/main.tsv --bin data/main.pyd
```

## 提交前验证指令

提交变更前，请在本地完整运行以下静态检查与单元测试：

```sh
# 格式化与静态检查
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo doc --no-deps --workspace --locked

# 核心协议与模块单元测试
cargo test -p lueying-protocol --lib --locked
cargo test -p lueying-extjs -p lueying-extmeta --lib --locked
cargo test -p lueying-bypass --locked
cargo test -p lueying-cli --lib --locked
cargo test -p lueying-server --lib --release --locked
```

### Windows 全链路端到端验证

在 Windows 环境下验证真实的输入法宿主注入与安装包打包逻辑：

```sh
cargo run -p lueying-e2e --release --locked -- --dict data/main.tsv --gram data/main.gram --rerank-dir data/rerank --bench-fast
cargo run -p lueying-win-e2e --release --locked
cargo run -p lueying-install-e2e --release --locked
```

## 代码与设计规范

1. **平台解耦**：词典、分词引擎、协议编解码、图形排版与扩展沙箱保持纯粹的跨平台属性；Windows API 与 COM 互操作严格封装在 `lueying-tsf` 与 `lueying-server` 中。
2. **源码精简**：保持单文件聚焦，单个 Rust 源码文件尽量控制在 1000 行内；通过 `#[path = "..."] mod ...;` 维持模块的清晰与平铺。
3. **依赖安全**：新增依赖仅允许来自 crates.io，严禁直接引用未经锁定的外部 Git 仓库。
4. **安全边界**：`unsafe` 代码仅允许出现在操作系统 FFI 边界，且必须在代码注释中明确列出前置不变式（Safety invariants）。
5. **文档同步**：修改通信协议、扩展契约、配置项或文件存储路径时，必须同步更新 `docs/` 下的对应文档。
6. **更新日志**：任何用户可见的功能增删或修复，请在 `CHANGELOG.md` 的 `[Unreleased]` 节中记录。

## Pull Request 规范

- 遵循单一职责：每个 PR 专注解决一个明确的问题或新增一个特性。
- 提交信息采用语义化前缀，如 `修复: ...`、`特性: ...`、`文档: ...`、`优化: ...`。
- 在 PR 说明中列出本地实际运行并通过的测试指令与结果截图。
- 若发现安全漏洞，请勿直接公开创建 Issue，请遵循 [安全策略](SECURITY.md) 提交私密报告。

## 开源许可证

本项目核心源码采用 GPL-3.0-or-later 协议授权。所包含的词典与模型权重分别遵循原作者开源许可协议。

# 参与开发

掠影是 Windows 平台的 TSF 拼音输入法，核心逻辑保持平台无关，Windows 专属代码集中在 TSF 与常驻服务边界。开始改代码前先读 [架构](docs/architecture.md) 与 [扩展 SDK](docs/extensions/README.md)。

仓库根的 `AGENTS.md` 是自动化代理的工作约束；人类贡献者按本文件构建与测试。

## 环境

- Windows 10 1809+ 或 Windows 11，x64。
- Rust 版本以 `rust-toolchain.toml` 为准，组件包含 `rustfmt` 与 `clippy`。
- 编安装包需要 Inno Setup 6.7.3。
- Linux 可跑引擎、扩展与渲染 E2E，需要 Skia 预编译包依赖的 `libfreetype6-dev`、`libfontconfig1-dev`。

词典与模型不入库，按钉住的提交与 SHA-256 拉取：

```sh
bash tools/fetch_frost.sh data/frost
bash tools/fetch_gram.sh data
bash tools/fetch_rerank.sh data/rerank
bash tools/fetch_ppocr.sh data/ppocr
cargo run -p xtask --release --locked -- dict --frost data/frost --out data/main.tsv --bin data/main.pyd
```

## 提交前检查

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo doc --no-deps --workspace --locked
cargo test -p lueying-protocol --lib --locked
cargo test -p lueying-extjs -p lueying-extmeta --lib --locked
cargo test -p lueying-bypass --locked
cargo test -p lueying-cli --lib --locked
cargo test -p lueying-server --lib --release --locked
```

Windows 上再跑真实宿主链路：

```sh
cargo run -p lueying-e2e --release --locked -- --dict data/main.tsv --gram data/main.gram --rerank-dir data/rerank --bench-fast
cargo run -p lueying-win-e2e --release --locked
cargo run -p lueying-install-e2e --release --locked
```

E2E 是主路径，单元测试补纯逻辑边界。新增行为先加场景或断言，再改实现。

## 代码约定

- 平台无关的词典、引擎、协议、渲染与扩展逻辑放在对应 crate；Windows API 调用留在 `lueying-tsf` 与 `lueying-server`。
- 单个 Rust 文件目标 1000 行以内；拆文件用 `#[path = "子文件.rs"] mod 子名;`，保持 `src/` 平铺。
- 新增依赖先确认 `Cargo.lock` 只来自 crates.io；CI 会拒绝 git 与未知 registry。
- `unsafe` 只出现在平台边界，必须写清调用前提。
- 改动管道协议、扩展契约、配置键或用户数据格式时，同步更新 `docs/architecture.md` 或对应扩展文档。
- 用户可见的变化写进 `CHANGELOG.md` 的 `Unreleased` 段。
- 不提交 `target/`、`data/main.*`、模型权重、安装包或本地日志。

## 提交与 PR

- 一个 PR 做一件事，说明要解决的问题、做法与验证结果。
- 提交信息用中文动词开头，例如 `修复: ...`、`文档: ...`、`性能: ...`。
- PR 里贴出实际跑过的命令；没有跑的部分明确写出来。
- 安全漏洞不要开公开 issue，按 [SECURITY.md](SECURITY.md) 私下报告。

## 许可证

贡献的代码按 GPL-3.0-or-later 授权。随包词库、模型与第三方数据各有许可，新增数据前先确认来源与署名要求。
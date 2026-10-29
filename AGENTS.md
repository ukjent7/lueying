- NEVER write unit tests after you write code. 
- Highly prefer E2E tests as the sole testing mechanism. Use them to verify complex features work. At the end of E2E tests, produce a verifiable and repeatable artifact. 
- If you must test a system in isolation, FIRST write all the ways it could fail, THEN write the code.

## 环境与工作区规范

- **禁止本地工具链**：本机仅负责代码编辑，**严禁私自下载、安装或调用 Rust 工具链**。编译、格式检查、Clippy 与测试一律交由 GitHub Actions 执行；仅在用户明确要求时才需核对 CI 状态。
- **单分支策略**：所有改动直接在 `main` 分支进行，严禁另起新分支。
- **工作区纯净**：非公开或参考文件一律存放在 `/home/kk/lueying-ime/work`，严禁在仓库根目录及源码目录乱存散落文件。
- **禁止笔记污染**：禁止在项目目录中新建 Markdown 等笔记/草稿文件；更推荐使用 grok CLI的 memory 功能。

## 代码与设计规范

- **最新稳定版**：Rust 与第三方依赖、工具链配置均紧跟最新稳定版本。
- **极简主义实现**：以最少、最清晰、最优雅的代码满足完整需求，坚决避免过度工程。
- **README 约束**：`README.md` 刻意保持极简，默认不改，能不改就不改。
- **自研主导与务实借鉴**：核心架构与输入方案坚持全自研；外部项目（微信输入法、水杉、清风等）主要作为场景兼容参考与成熟方案借鉴源。拒绝边际效应极低的盲目自研

## 规则优先级与演进原则

- **最高权威**：用户不会主动要求违反 `AGENTS.md`。若判定用户要求与本规则冲突，一律以 `AGENTS.md` 为准；严禁主动修改本文件。
- **无包袱重构**：严禁在不良设计上修修补补。只要发现方案落后、代码别扭，任何部分均不计沉没成本，主动、果断地推翻重构或彻底重写。

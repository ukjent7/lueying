- NEVER write unit tests after you write code. 
- Highly prefer E2E tests as the sole testing mechanism. Use them to verify complex features work. At the end of E2E tests, produce a verifiable and repeatable artifact. 
- If you must test a system in isolation, FIRST write all the ways it could fail, THEN write the code.

## 开发环境与验证

- 本机仅负责代码编写，不安装 Rust 工具链。**严禁私自下载或使用rust 工具链**
- Rust 编译、格式检查、静态检查和测试交由 GitHub Actions 执行。
- 不必每次提交都验证CI是否全绿，只有用户要求才需要验证
- 禁止另开分支，所有改动都在main进行

## Rust 与代码规范

- Rust 工具链和依赖/各种工具均使用最新稳定版。
- 使用尽量少的代码实现完整需求，优先选择简洁、清晰、优雅的方案。
- `README.md` 刻意保持极简。**默认不要改它，能不改就不改。**
- 尽量参考水杉、清风、青简这三个rust项目现有做法
- 更推荐参考大厂微信输入法

## 用户不会主动要求违反AGENTS.MD的规则，如果你认为用户要求和AGENTS.MD规则冲突，以AGENTS.MD规则为准
- 禁止主动修改AGENTS.MD

## 项目处于快速迭代阶段，任何有利的改动都值得不计成本的投入，不要有历史包袱负担，允许随时进行任何程度的重写、重构

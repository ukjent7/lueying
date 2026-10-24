- NEVER write unit tests after you write code. 
- Highly prefer E2E tests as the sole testing mechanism. Use them to verify complex features work. At the end of E2E tests, produce a verifiable and repeatable artifact. 
- If you must test a system in isolation, FIRST write all the ways it could fail, THEN write the code.

## 开发环境与验证

- 本机仅负责代码编写，不安装 Rust 工具链。
- Rust 编译、格式检查、静态检查和测试交由 GitHub Actions 执行。

## Rust 与代码规范

- Rust 工具链和依赖/各种工具均使用最新稳定版。
- 编写 Rust 代码前，参考工作区 `edition-guide` 中的 Rust 2024 现代推荐写法。
- 使用尽量少的代码实现完整需求，优先选择简洁、清晰、优雅的方案。
- `README.md` 刻意保持极简。**默认不要改它，能不改就不改。**
- 尽量参考水杉、清风、青简这三个rust项目现有做法
- 更推荐参考大厂微信输入法
# 掠影技术文档

掠影（Lueying）是基于 Windows TSF 与 Rust 实现的现代中文拼音输入法。本文档集涵盖从用户日常使用、外观定制到内核架构与扩展开发的完整说明。

## 使用与配置

- [安装与卸载](install.md)：系统要求、安装流程、升级逻辑与数据目录。
- [使用指南](usage.md)：基础按键、标点输入、符号模式与候选词管理。
- [详细设置](settings.md)：设置中心功能、`config.toml` 配置项速查与按应用配置。
- [界面皮肤](skin.md)：声明式配色配置与颜色键说明。
- [用户效果](effects.md)：基于 SkSL 的着色器装饰与动态效果契约。
- [故障排查](troubleshooting.md)：常见问题解决、日志收集与问题报告指南。

## 架构与演进

- [系统架构](architecture.md)：进程模型、引擎解码、渲染管线与质量门禁。
- [候选基准](benchmark.md)：评测语料集、评测标准、复现方法与延迟约束。
- [路线规划](roadmap.md)：设计哲学、核心状态与后续特性规划。
- [版本发布](release.md)：发布前检查、Tag 触发流程、签名规范与验证回退。

## 扩展开发

- [扩展 SDK 概览](extensions/README.md)：扩展分层、Rust 扩展契约与最小示例。
- [分层架构模型](extensions/layers.md)：扩展运行位置、沙箱设计与权限审计模型。
- [运行期脚本扩展](extensions/script.md)：QuickJS 脚本扩展规范、清单定义、API 与服务调用。
- [双拼方案](extensions/shuangpin.md)：双拼展开规则、映射表与多方案支持。
- [辅助码](extensions/auxiliary-code.md)：五笔辅码过滤规则与输入交互。
- [自定义短语](extensions/custom-phrases.md)：短语映射格式与匹配策略。
- [下一词联想](extensions/prediction.md)：基于用户词库的本地联想机制。
- [外部启动器集成](extensions/powertoys.md)：PowerToys Run 与外部启动器联动桥接。
- [Agent 开发指南](extensions/agent-guide.md)：面向 AI Coding Agent 的无头开发与回放验证。
- [扩展开发 Skill 模板](extensions/SKILL.md)：符合规范的扩展包编写模板与验收清单。

## 项目规范

- [更新日志](../CHANGELOG.md)：版本演进历史与重要更新记录。
- [开发参与指南](../CONTRIBUTING.md)：环境搭建、代码规范、提交与 PR 流程。
- [安全策略](../SECURITY.md)：漏洞披露渠道、响应流程与安全设计边界。
- [行为准则](../CODE_OF_CONDUCT.md)：社区交流与协作准则。

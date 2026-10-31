## 改动

说明要解决的问题与做法。

## 验证

贴出实际跑过的命令与结果；没跑的部分写清原因。

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] 相关单元测试
- [ ] 相关 E2E（引擎 / Windows 宿主 / 安装）

## 风险

协议、配置、用户数据、扩展契约或安装升级有变化时，说明兼容性与回退方式。

## 检查

- [ ] 用户可见变化已写入 `CHANGELOG.md` 的 `Unreleased`
- [ ] 文档与代码一致
- [ ] 没有提交 `target/`、模型、词库或本地日志
- [ ] 没有真实用户输入内容、密码或个人信息
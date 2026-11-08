# 掠影

掠影是基于 Windows TSF 与 Rust 构建的中文拼音输入法。

## 特性

- **解码**：全拼与主流双拼（小鹤、自然码、微软、搜狗），音节图与词图解码，支持整句输入。
- **排序**：词频、语法搭配与语言模型重排的多层排序，并随选词自学习。
- **界面**：Direct2D 无焦点候选窗，适配高对比度与「减少动态效果」。
- **扩展**：Wasm 沙箱扩展与原生扩展，权限需用户同意后生效。
- **隐私**：词典、词库与配置默认只存本机。核心输入不联网；只有用户主动开启的 WebDAV 同步才会上传。

## 安装

- 系统要求：Windows 11 24H2（build 26100）及以上，x64。
- 从 [Releases](https://github.com/ukjent7/lueying/releases) 下载 `lueying-<版本>-setup.exe`，以管理员身份运行。
- 按 <kbd>Win</kbd> + <kbd>Space</kbd>，切换到「掠影」。

## 基本操作

空格上屏首选，数字键选字，`-` / `=` 翻页，单击 <kbd>Shift</kbd> 切换中英文。完整键位与快捷输入见[使用指南](docs/usage.md)。

## 配置

在开始菜单打开「掠影设置」。配置文件位于 `%APPDATA%\lueying\config.toml`，字段说明见[设置](docs/settings.md)。

## 平台

Windows 是主要平台。

## 文档

- [文档索引](docs/README.md)
- [故障排查](docs/troubleshooting.md)

## 贡献与安全

参与开发见 [CONTRIBUTING.md](CONTRIBUTING.md)。安全问题请按 [SECURITY.md](SECURITY.md) 私下报告。

## 许可证

[GPL-3.0-or-later](LICENSE)

# Wasm 扩展

Wasm 扩展是编译为 Wasm 模块的插件，由 Extism 宿主加载，运行在 `crates/extwasm` 的沙箱中。

## 清单

`kind` 默认为 `wasm`，`wasm` 指向包内的模块文件。模块文件不超过 16 MiB。清单的完整字段见[扩展总览](README.md)。

## 导出

- 提供候选外观时（清单 `appearance: true`），模块必须导出 `lueying_appearance`，返回主题色补丁。
- 命令入口由清单 `activate.commands` 声明。

外观函数只能读取面板状态并返回颜色补丁，不能取得候选汉字、按键历史或应用名称。

## 宿主函数

宿主只向模块链接清单 `needs` 中声明的服务。服务与 Rust 侧的封装都在 `crates/extguest` 中：

`lueying_timeout`、`lueying_capture`、`lueying_ocr`、`lueying_output`、`lueying_panel`、`lueying_clipboard`、`lueying_store`、`lueying_notify`、`lueying_hotkey`、`lueying_network`、`lueying_context`、`lueying_audio`、`lueying_asr`。

## 资源限制

| 资源 | 限额 |
|---|---|
| 线性内存 | 16 MiB |
| 值栈 | 256 KiB |
| 初始化 / 命令 / 外观燃料 | 5000 万 / 2 亿 / 2000 万 |

超限的调用被中断。连续 3 次故障后扩展停用。完整说明见[分层与沙箱](layers.md)。

## 参考

`extensions/clipboard-history` 是一个完整的 Wasm 扩展示例，包含清单、热键与面板。

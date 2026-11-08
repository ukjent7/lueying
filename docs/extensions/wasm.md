# Wasm 扩展

Wasm 扩展是打包成 WebAssembly 组件的插件，由 wasmtime 加载，运行在 `crates/extwasm` 的沙箱中。

## 清单

`kind` 默认为 `wasm`，`wasm` 指向包内的组件文件。组件文件不超过 16 MiB。清单的完整字段见[扩展总览](README.md)。

## 导出

组件按 WIT 契约 `lueying:ext@2.0.0`（`crates/extwasm/lueying.wit`）导出 `entry` 接口：

- `init()`：加载后调用一次，返回错误则加载失败。
- `command(id, args)`：命令入口，参数与返回值都是 JSON 文本；命令 id 由清单 `activate.commands` 声明。
- `appearance(scene)`：提供候选外观时（清单 `appearance: true`）被派发，返回主题色补丁 JSON 或空。

外观函数只能读取面板状态并返回颜色补丁，不能取得候选汉字、按键历史或应用名称。

## 宿主服务

宿主只向组件链接清单 `needs` 中声明的服务接口；用到没声明的服务在实例化期就被拒绝。服务与 Rust 侧的封装都在 `crates/extguest` 中（`capture`、`ocr`、`clipboard`、`store`、`output`、`panel`、`hotkey`、`notify`、`network`、`context`、`audio`、`asr`）。

## 打包

```sh
cargo build --release -p my-extension --target wasm32-unknown-unknown
wasm-tools component new target/wasm32-unknown-unknown/release/my_extension.wasm -o extension.wasm
```

## 资源限制

| 资源 | 限额 |
|---|---|
| 线性内存 | 16 MiB |
| 值栈 | 256 KiB |
| 初始化 / 命令 / 外观燃料 | 5000 万 / 2 亿 / 2000 万 |

超限的调用被中断。连续 3 次故障后扩展停用。完整说明见[分层与沙箱](layers.md)。

## 参考

`extensions/clipboard-history` 是一个完整的 Wasm 扩展示例，包含清单、热键与面板。

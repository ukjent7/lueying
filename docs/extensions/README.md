# 扩展

掠影的功能可以通过三种方式接入，按能力与信任边界区分：

| 类型 | 形态 | 适合 |
|---|---|---|
| 编译期扩展 | Rust 模块，随内核编译 | 输入方案、候选模块（如快捷输入、联想） |
| Wasm 扩展 | Wasm 组件，由 wasmtime 加载 | 候选外观、命令、声明式设置 |
| 原生扩展 | 独立可执行文件 | 较重的系统能力，如 OCR、语音识别；也可订阅候选窗状态 |

官方扩展随发布提供：`shot-ocr`（截图识别）、`clipboard-history`（剪贴板历史）、`voice-asr`（语音输入）、`input-schemes`（输入方案），以及原生的 `ppocr` 与 `sensevoice`。下载地址与哈希见 `Catalog/official-extensions.json`。

## 清单

每个扩展目录包含 `manifest.json`，目录名即扩展 id。以官方剪贴板历史为例：

```json
{
  "id": "clipboard-history",
  "name": "剪贴板历史",
  "version": "0.1.0",
  "api": 4,
  "wasm": "extension.wasm",
  "activate": {
    "hotkeys": ["Win+Shift+V"],
    "commands": [
      { "id": "show", "title": "剪贴板历史", "when": "any" }
    ]
  },
  "needs": ["clipboard", "panel", "output", "notify"],
  "capabilities": ["clipboard"]
}
```

| 字段 | 说明 |
|---|---|
| `id`、`name`、`version` | 扩展 id（同时是目录名）、显示名、扩展自身版本 |
| `api` | 清单 API 版本，当前为 `4` |
| `kind` | `wasm`（默认）或 `native` |
| `wasm` / `exe` | 包内的模块文件（wasm）或可执行文件（native） |
| `activate` | `triggers` 触发词、`hotkeys` 建议热键、`commands` 命令（含 id、标题与出现时机） |
| `needs` | 需要的服务。宿主只链接这里声明的导入 |
| `capabilities` | 申请的权限，用于授权页与同意记录 |
| `events` | 订阅的宿主事件。目前只有 `candidate-state`，且仅原生扩展可用 |
| `network` | `allow` 域名白名单。`needs` 含 `network` 时必填 |
| `appearance` | 是否提供候选窗外观；为 `true` 时组件的 `appearance` 导出会被派发 |
| `settings` | 声明式设置，值保存在 `config.toml` 的 `[ext.<id>]` 下 |

## 服务（needs）

`capture`、`ocr`、`clipboard`、`panel`、`output`、`hotkey`、`store`、`notify`、`network`、`context`、`audio`、`asr`。

网络服务只经独立的 `lueying-broker.exe` 访问，且只能到清单白名单内的域名。

## 权限（capabilities）

`candidate-text`（候选文字）、`screen-capture`（截屏）、`ocr`、`clipboard`、`selected-text`（选中文字）、`audio`（录音）。

首次启用时在「设置 → 扩展」中查看并同意。详见[分层与权限](layers.md)。

## 命令行

| 命令 | 作用 |
|---|---|
| `lueying ext commands` | 列出已启用扩展的命令 id |
| `lueying ext run <命令 id>` | 执行一个命令，如 `shot-ocr.run`；见[启动器集成](powertoys.md) |
| `lueying ext add github:作者/仓库#版本` | 一条命令安装社区扩展，见[社区目录](catalog.md) |
| `lueying ext reload` | 重新加载配置与皮肤，扩展即时生效 |
| `lueying ext top` | 查看扩展运行指标 |
| `lueying ext new`、`pack`、`install` | 创建、打包与安装扩展 |

Wasm 扩展的开发方式见 [wasm.md](wasm.md)；Rust 之外的语言（JS/Python/C/Go）见
[languages.md](languages.md)；安装与发布社区扩展见 [catalog.md](catalog.md)。

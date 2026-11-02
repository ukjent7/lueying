# 运行期 Wasm 扩展规范

运行期扩展是一份 `manifest.json` 加一个核心 Wasm 模块。宿主用 wasmtime
(Cranelift JIT)在常驻服务进程内执行:线性内存隔离、燃料计量、资源限额,
没有独立进程。扩展作者用 Rust(或其它能产出 `wasm32-unknown-unknown`
的语言)实现同步的 `Extension` trait,服务调用阻塞的是扩展自己的
worker 线程,绝不触碰输入热路径。

## 包结构

```
shot-ocr/
├── manifest.json       # 清单(API 3)
└── extension.wasm      # wasm32-unknown-unknown 编译产物
```

目录名必须等于清单 `id`。`.luext` 分发包是 `### lueying-pack v2` 格式:
文本块按行数声明长度,`.wasm` 按字节数原样内嵌,`lueying ext pack/install`
负责打包与安装。

## 清单(API 3)

```json
{
  "id": "shot-ocr",
  "name": "截图 OCR",
  "version": "0.1.0",
  "api": 3,
  "wasm": "extension.wasm",
  "activate": {
    "triggers": ["ocr"],
    "hotkeys": ["Ctrl+Alt+O"],
    "commands": [
      { "id": "run", "title": "截图并识别文字", "when": "any" }
    ]
  },
  "needs": ["capture", "ocr", "output", "notify"],
  "capabilities": ["screen-capture", "ocr"],
  "settings": []
}
```

- `wasm`:Wasm 模块路径,包内裸文件名。
- `appearance`:为 `true` 时模块必须导出 `lueying_appearance`,包常驻运行。
- `needs`:宿主只链接这里声明过的服务导入;没声明的服务在实例化时直接
  加载失败。
- `network`:联网白名单,`needs` 声明 `network` 时必填。条目是 `host` 或
  `host:port`(如 `"api.example.com"`、`"dev.local:8080"`),精确匹配,
  不收通配符与 scheme,1..=32 条;白名单进同意书,改一条就要重新同意。
- `capabilities` 与 `needs` 非空时,用户必须先同意;同意记录对
  `manifest.json` 与 `.wasm` 的 SHA-256 做指纹,任一变动都要求重新同意。
- 触发词、建议热键与命令只是声明;实际绑定关系由用户的 `config.toml` 决定。

## Guest SDK

依赖工作区发布的 `lueying-extguest`(骨架由 `lueying ext new` 生成):

```rust
use lueying_extguest::{Extension, capture_shot, notify_show, ocr_recognize, output_deliver};
use serde_json::json;

#[derive(Default)]
struct ShotOcr;

impl Extension for ShotOcr {
    fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
        if id != "run" {
            return Err(format!("没有命令 {id}"));
        }
        let shot = capture_shot()?;
        let ocr = ocr_recognize(&shot.file, None)?;
        let delivered = output_deliver(&ocr.text)?;
        Ok(json!({ "chars": ocr.text.chars().count(), "delivered": delivered.delivered }).to_string())
    }
}

lueying_extguest::export!(ShotOcr);
```

`Extension` 三个方法都有默认实现:`init`(加载失败可报错)、`command`
(命令入口,参数与返回值都是 JSON 文本)、`appearance`(每帧外观补丁)。
服务封装全部是同步函数:`capture_shot`、`ocr_recognize`、`output_deliver`、
`panel_show`、`clipboard_read/write`、`store_get/set/remove/list`、
`notify_show`、`hotkey_register/unregister/press`、`network_fetch` 与
`timeout_sleep`。`network_fetch` 收 `FetchArgs`(与 WIT `fetch-options`
同形),域名必须在清单白名单里,响应体按 UTF-8 文本交付。

构建与发布:

```sh
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/<crate>.wasm extension.wasm
lueying ext pack <dir>     # 产出 .luext
lueying ext install <file> # 装进扩展目录
lueying ext reload         # 常驻服务热重载
```

## ABI

guest 导出(全部由 SDK 宏生成,手写 guest 才需要关心):

| 导出 | 签名 | 说明 |
|---|---|---|
| `memory` | — | 唯一线性内存 |
| `lueying_init` | `() -> i32` | 0 成功;非 0 时经 `emit` 交错误文本 |
| `lueying_command` | `(id ptr, id len, args ptr, args len) -> i32` | 0 成功 |
| `lueying_appearance` | `(scene ptr, scene len) -> i32` | 0 成功;空结果表示这一帧不改 |
| `lueying_alloc` / `lueying_free` | `(len) -> ptr` / `(ptr, len)` | 宿主写输入用 |
| `lueying_result_ptr` / `lueying_result_cap` | `() -> i32` | 服务结果缓冲区 |

宿主导入(模块为 `lueying`;只链接清单 `needs` 里声明过的服务):

| 导入 | 签名 | 说明 |
|---|---|---|
| `emit` | `(ptr, len, ok)` | 交回命令/外观的最终结果 |
| `timeout` | `(ms) -> i32` | 可取消睡眠;扩展卸载时返回 -1 |
| `capture` / `ocr` / `output` / `panel` / `clipboard` / `store` / `hotkey` / `notify` / `network` | `(method ptr, method len, args ptr, args len) -> i32` | 返回写进结果缓冲区的字节数,-1 表示失败 |

服务结果是一个 JSON 信封:成功 `{"ok":true,"value":...}`,失败
`{"ok":false,"error":"..."}`。宿主把信封写进 guest 的结果缓冲区,
返回写入长度;超长时宿主直接回一个错误信封。

## 资源限额与熔断

| 资源项 | 限额 | 超限处理 |
|---|---|---|
| 线性内存 | 16 MiB | 增长即 trap,计一次 OOM |
| Wasm 值栈 | 256 KiB | 栈溢出 trap |
| 初始化燃料 | 50 M | trap,加载失败 |
| 命令燃料 | 200 M | trap,计一次超时 |
| 外观燃料 | 20 M | trap,计一次超时 |
| 宿主服务等待 | 面板 600 s、联网 140 s、OCR 120 s、截图 30 s、其余 10 s | 调用取消,错误信封回给 guest |
| 命令空闲卸载 | 30 s | worker 与实例整体丢弃;外观包常驻 |
| 容错熔断 | 连续 3 次失败 | 扩展停用,重新加载或热重载成功后恢复 |

trap 之后实例不可再用,下一次调用前宿主自动重建;正在执行的阻塞服务
调用在卸载或停用时 50 ms 内取消。

## 安全与隐私边界

- guest 看不到候选汉字、按键历史与宿主应用名;场景状态只有页码、候选
  数量、主题色与系统外观开关。
- 密码/隐私字段下:外观不派发、命令与热键不执行、服务调用全部拒绝。
- 联网只经独立 broker 进程 `lueying-broker.exe`:宿主提供者逐请求核对
  清单白名单,broker 兜传输的底(HTTPS-only 外部地址、响应体默认 256 KiB
  封顶 512 KiB、时限封顶 120 s);`lueying-server.exe` 保持离线(CI 导入表
  断言)。外观帧不做联网调用,长请求放命令里。
- 所有服务调用写审计日志(服务、方法与结果),不记录用户正文。
- 外观补丁只有主题色字段;高对比度下仍返回主题色。

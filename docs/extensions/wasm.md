# 运行期 Wasm 扩展规范

运行期扩展是一份 `manifest.json` 加一个核心 Wasm 模块。模块就是标准
[Extism](https://extism.org) 插件:宿主用 Extism 运行期(Cranelift JIT)在
常驻服务进程内执行——线性内存隔离、燃料计量、资源限额,没有独立进程。
别的 Extism 宿主写的插件、官方 Extism PDK 编出来的插件,放进扩展目录
同样能被调用。

扩展作者用 Rust(或其它能产出 `wasm32-unknown-unknown` 的语言)实现同步
的 `Extension` trait,服务调用阻塞的是扩展自己的 worker 线程,绝不触碰
输入热路径。

## 第三方插件怎么接进来

官方 Extism PDK 编出的插件直接能用,不用改代码:

- 导出是 `() -> i32`:输入从内核的 `input_length`/`input_load_u8` 读,
  结果用 `output_set` 交回。宿主按清单里的命令 id 调同名导出,所以清单里
  把命令 id 写成插件的导出名即可。
- PDK 自带的能力由 Extism 运行期提供:日志进服务端日志,变量与配置照
  Extism 的语义走,`http_request` 被拒——插件自己发不出 HTTP,联网只走
  清单白名单里的 `network` 服务。
- 插件里要调掠影的服务原语(截图、识别、上屏等),按本文 ABI 从
  `extism:host/user` 导入 `lueying_<服务>`;用 Rust 就直接依赖
  `lueying-extguest`。

## 包结构

```
shot-ocr/
├── manifest.json       # 清单(API 4)
└── extension.wasm      # wasm32-unknown-unknown 编译产物
```

目录名必须等于清单 `id`。`.luext` 分发包是 `### lueying-pack v2` 格式:
文本块按行数声明长度,`.wasm` 按字节数原样内嵌,`lueying ext pack/install`
负责打包与安装。

## 清单(API 4)

```json
{
  "id": "shot-ocr",
  "name": "截图 OCR",
  "version": "0.1.0",
  "api": 4,
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
- `capabilities` 里的 `selected-text` 是读当前窗口选中文字的门票;没申请
  就调不到 `context` 服务,每一级上下文单独同意。
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

lueying_extguest::export!(ShotOcr, commands: [run]);
```

`export!` 的 `commands` 列出命令 id,每个 id 生成一个同名导出:宿主按
清单里的命令 id 调用它,输入是参数 JSON,输出是结果 JSON。命令 id 就是
导出名,所以第三方 Extism 插件(导出名任意)也能用:清单里把命令 id 写成
它的导出名即可。

`Extension` 三个方法都有默认实现:`init`(加载失败可报错)、`command`
(命令入口,参数与返回值都是 JSON 文本)、`appearance`(每帧外观补丁)。
服务封装全部是同步函数:`capture_shot`、`ocr_recognize`、`output_deliver`、
`panel_show`、`clipboard_read/write/history/remember/clear`、`store_get/set/remove/list`、
`notify_show`、`hotkey_register/unregister/press`、`network_fetch`、
`context_selection`、`audio_record`、`asr_recognize` 与 `timeout_sleep`。`network_fetch` 收 `FetchArgs`
(与 WIT `fetch-options` 同形),域名必须在清单白名单里,响应体按 UTF-8
文本交付;`context_selection` 读当前窗口选中文字,清单要申请
`selected-text` 权限。

不想写 Rust 时按下面的 ABI 手写 guest 即可:`crates/extguest/include/lueying_ext.h`
是同一份契约的 C 头文件,含导入声明与导出宏;`crates/extguest/c/example.c`
是能直接编的最小例子:

```sh
clang --target=wasm32-unknown-unknown -nostdlib -O2 \
  -I crates/extguest/include \
  -Wl,--no-entry -Wl,--export-memory \
  -o extension.wasm crates/extguest/c/example.c
```

任何能产出 `wasm32-unknown-unknown` 的工具链都能用这套契约。

构建与发布:

```sh
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/<crate>.wasm extension.wasm
lueying ext pack <dir>     # 产出 .luext
lueying ext install <file> # 装进扩展目录
lueying ext reload         # 常驻服务热重载
```

## ABI

数据都在 Extism 的内存里:guest 从 `extism:host/env` 导入内核函数
(分配、读写、输入输出、错误),不自己管线性内存。内核函数清单:

| 导入 | 签名 | 说明 |
|---|---|---|
| `alloc` / `free` | `(len) -> offset` / `(offset)` | 分配与释放一块内存 |
| `length` | `(offset) -> len` | 取块长度 |
| `load_u8` / `store_u8` | `(offset) -> u8` / `(offset, u8)` | 逐字节读写 |
| `input_length` / `input_load_u8` | `() -> len` / `(index) -> u8` | 读宿主传进来的输入 |
| `output_set` | `(offset, len)` | 交回结果 |
| `error_set` | `(offset)` | 交回错误文本 |

guest 导出(SDK 宏生成;手写 guest 自己定名):

| 导出 | 签名 | 说明 |
|---|---|---|
| `<命令 id>` | `() -> i32` | 每个命令一个;输入是参数 JSON,输出是结果 JSON |
| `lueying_appearance` | `() -> i32` | 清单 `appearance` 为 true 时必须实现;输入是外观状态 JSON |
| `lueying_init` | `() -> i32` | 可选;有就调一次,失败即加载失败 |

宿主导入(模块为 `extism:host/user`;只链接清单 `needs` 里声明过的服务):

| 导入 | 签名 | 说明 |
|---|---|---|
| `lueying_timeout` | `(ms) -> i32` | 可取消睡眠;扩展卸载时返回 -1 |
| `lueying_capture` / `lueying_ocr` / `lueying_output` / `lueying_panel` / `lueying_clipboard` / `lueying_store` / `lueying_hotkey` / `lueying_notify` / `lueying_network` / `lueying_context` / `lueying_audio` / `lueying_asr` | `(request offset) -> offset` | 收一段 JSON 请求,回一段 JSON 响应 |

服务请求是 `{"method":…,"args":…}`,响应是 `{"ok":true,"value":…}` 或
`{"ok":false,"error":"…"}`。请求里的 `args` 原样转给服务提供者。

服务失败不静默:错误信封交给 guest,guest 自己失败时用它的错误文本;guest
收到信封仍返回成功,宿主按这条服务错误把这次调用判失败——扩展吞不掉宿主
给不出的服务。

这份 ABI 是扩展与宿主之间的唯一契约:导入模块名、导出符号名与签名都固定,
契约本身不含任何运行时细节,宿主换一个 JIT 也不影响扩展。清单 `api` 字段
标出契约版本,版本对不上宿主直接拒绝加载。

## 资源限额与熔断

| 资源项 | 限额 | 超限处理 |
|---|---|---|
| 线性内存 | 16 MiB(Extism 页上限) | 增长失败即 trap,计一次 OOM |
| Wasm 值栈 | 256 KiB | 栈溢出 trap |
| 初始化燃料 | 50 M | 实例化阶段烧完即 trap,加载失败 |
| 命令燃料 | 200 M | trap,计一次超时 |
| 外观燃料 | 20 M | trap,计一次超时 |
| 宿主服务等待 | 面板 600 s、联网 140 s、OCR/ASR 120 s、录音 40 s、截图 30 s、其余 10 s | 调用取消,错误信封回给 guest |
| 命令空闲卸载 | 30 s | worker 与实例整体丢弃;外观包常驻 |
| 容错熔断 | 连续 3 次失败 | 扩展停用,重新加载或热重载成功后恢复 |

trap 之后实例照旧能用,下一次调用前 Extism 重置内核状态;正在执行的阻塞
服务调用在卸载或停用时 50 ms 内取消。

## 安全与隐私边界

- guest 看不到候选汉字、按键历史与宿主应用名;外观状态只有页码、候选
  数量、主题色与系统外观开关。
- 密码/隐私字段下:外观不派发、命令与热键不执行、服务调用全部拒绝。
- 联网只经独立 broker 进程 `lueying-broker.exe`:宿主提供者逐请求核对
  清单白名单,broker 兜传输的底(HTTPS-only 外部地址、响应体默认 256 KiB
  封顶 512 KiB、时限封顶 120 s);`lueying-server.exe` 保持离线(CI 导入表
  断言)。插件自己发不出 HTTP:Extism 运行期没开联网特性,`http_request`
  直接被拒。外观帧不做联网调用,长请求放命令里。
- 上下文分级:只有清单申请 `selected-text` 的扩展能读当前窗口选中文字;
  密码框与输入法自家窗口一律不读,读不到就是读不到。
- 所有服务调用写审计日志(服务、方法与结果),不记录用户正文。
- 外观补丁只有主题色字段;高对比度下仍返回主题色。

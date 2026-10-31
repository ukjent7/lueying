# 扩展 SDK 概览

掠影输入法遵循「极简核心、模块化扩展」的设计哲学。核心引擎专注负责规范全拼、音节图切分、候选生成与自学习；外延与个性化功能通过清晰的扩展机制接入。

## 扩展类型矩阵

| 扩展形态 | 核心契约 | 运行位置 | 是否改变音节切分 | 典型应用场景 |
|---|---|---|---|---|
| **输入方案** | `Scheme` | 核心引擎内（编译期注入） | **是** | 双拼、自定义键位、直接辅助码 |
| **候选模块** | `Module` | 候选管线中（编译期注入） | **否** | 自定义短语、下一词联想、候选重排 |
| **运行期脚本** | `QuickJS` 沙箱 | 独立工作线程（动态加载） | **否** | 候选窗动态装饰、命令动作、OCR 截图 |

### 核心分工说明

- **方案层（`Scheme`）**：负责将击键映射为规范全拼，并建立双向区间映射。无论使用何种方案，底层均复用统一的主词典、语言模型与用户词库。
- **模块层（`Module`）**：在基础候选与快捷输入生成后执行，支持插入、重排、筛选候选词或在上屏阶段改写文本。
- **脚本层（QuickJS）**：面向动态业务，沙箱运行且无法读取候选汉字与按键历史，确保隐私安全。

## 编译期 Rust SDK

扩展开发者依赖工作区中的 `crates/ext`（`lueying-ext`）：

```toml
[dependencies]
lueying-ext = { path = "crates/ext" }
```

### 核心公开符号

- **方案契约**：`Scheme`、`SchemeManifest`、`Expansion`、`CandidateFilter`。
- **模块契约**：`Module`、`Manifest`、`Capability`、`CandidateContext`、`CommitContext`。
- **数据结构**：`Candidate`、`Config`、`Engine`、`Key`、`UserDict`。

### 最小候选模块示例

```rust
use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};

pub struct DemoModule;

impl Module for DemoModule {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "demo",
            name: "演示模块",
            version: "1.0.0",
            capabilities: &[Capability::Candidates],
        }
    }

    fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        if ctx.query == "demo" {
            let mut cand = Candidate::whole(ctx.raw.len(), "演示内容", 1.0);
            cand.hint = Some("扩展".into());
            cands.insert(0, cand);
        }
    }
}
```

### 最小输入方案示例

```rust
use lueying_ext::{Expansion, Scheme, SchemeManifest};

pub struct UpperScheme;

impl Scheme for UpperScheme {
    fn manifest(&self) -> SchemeManifest {
        SchemeManifest {
            id: "upper",
            name: "大写转换方案",
            version: "1.0.0",
        }
    }

    fn expand(&self, raw: &str) -> Expansion {
        Expansion::identity(&raw.to_ascii_lowercase())
    }
}
```

## 运行期脚本与扩展 CLI

运行期脚本存放在 `%APPDATA%\lueying\extensions\<id>\`，通过 `manifest.json` 与 `index.js` 定义。

`lueying ext` 命令行工具提供全生命周期管理，在 Windows 与 Linux 环境下均可无头执行：

```sh
lueying ext new <id>           # 创建扩展标准骨架代码
lueying ext pack <dir>         # 将目录打包为纯文本 .luext 归档
lueying ext install <file>     # 安装 .luext 扩展包至扩展目录
lueying ext reload             # 通知常驻服务立即重读配置与扩展
lueying ext replay <script>    # 无头回放按键序列并输出候选判定
lueying ext logs --tail 50     # 实时查看服务端扩展日志
lueying ext top                # 监视扩展内存占用、派发耗时与故障计数
```

## 延伸文档

- [扩展分层与权限模型](layers.md)：深入了解沙箱隔离、服务声明与权限审计体系。
- [运行期脚本规范](script.md)：JavaScript 脚本扩展开发完整 API、清单与服务原语。
- [Coding Agent 开发指南](agent-guide.md)：面向 AI 与无头终端的扩展自动化开发测试指南。
- [PowerToys 联动集成](powertoys.md)：通过外部启动器无缝调用输入法扩展命令。

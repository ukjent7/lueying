# 扩展 SDK

掠影把高级、小众能力放在核心之外。核心引擎只负责全拼、音节图、候选生成与学习;扩展按信任边界分层,当前已落地编译期契约与外观脚本,逻辑 WASM 与进程外能力在推进中。分层设计、清单与权限见 [layers.md](layers.md)。

## 三类扩展

| 类型 | 契约 | 适用能力 | 是否改变音节切分 |
|---|---|---|---|
| 输入方案 | `Scheme` | 双拼、直接辅助码、自定义键盘布局 | 是 |
| 候选模块 | `Module` | 自定义短语、下一词联想、候选重排、上屏改写 | 否 |
| 运行期脚本 | `ScenePatch` | 候选窗主题色与 SkSL 装饰层 | 否 |

`Scheme` 把用户击键展开成规范全拼,并给出查询串与原始击键的双向位置映射。核心词图只认规范全拼,所以双拼与全拼共享同一份词典、语言模型和用户词库。候选的 `spans` 会映射回原始击键,回删、逐段上屏和候选高亮仍按用户实际敲的键工作。

`Module` 在核心候选与快捷输入之后运行,可以插入、删除、重排候选,或在最终上屏事件上改写文本。模块按注册顺序执行。

## 运行期脚本扩展

候选窗外观与命令可以交给一段 JS:包放在 `%APPDATA%\lueying\extensions\<id>\`,输入法在 QuickJS 沙箱里同步派发面板状态,脚本只回主题色与三个 SkSL 槽位。状态里没有候选文字与宿主名,密码/隐私字段不调用脚本,内存、栈、初始化与单次派发都有上限。扩展还可以声明命令(`lueying.command`),宿主按完整 id `<扩展>.<命令>` 调用,卸载时 Scope 整体回收。

包格式、场景字段、补丁、命令与限额见 [script.md](script.md)。服务端按 `config.toml` 的 `extensions` 列表按序加载多个扩展,派发在独立线程里,窗口线程只收主题色与 SkSL 源码;Windows CI 有运行时自检。设置页「扩展 → 运行期脚本」列出已安装的包,可直接启用或关闭;「目录」打开扩展目录。清单里的 `settings` 是声明式设置,由宿主渲染成开关与单选,值存 `[ext.<id>]`。

命令还可以调用清单 `needs` 里声明的宿主服务(`capture`、`ocr`、`output`、`hotkey`、`notify`),接口见 [script.md](script.md) 的「服务调用」。官方 `shot-ocr`(截图 → 识别 → 上屏)随安装包分发到 `%APPDATA%\lueying\extensions\shot-ocr`,是这些接口的参考实现。截图识别可在设置页「扩展 → 截图识别」里选系统 OCR 或随包的 PP-OCRv6;扩展也能在 `ocr.recognize` 调用里用 `engine` 单独指定。

## SDK crate

扩展作者依赖 `crates/ext`(`lueying-ext`),不需要直接依赖引擎内部模块:

```toml
[dependencies]
lueying-ext = { path = "crates/ext" }
```

`crates/extmeta`(`lueying-extmeta`)是包清单与权限模型,由宿主与工具使用,扩展作者通常不需要直接依赖。

公开入口:

- `lueying_ext::prelude::*`
- `Scheme`、`SchemeManifest`、`Expansion`、`CandidateFilter`
- `Module`、`Manifest`、`Capability`、`CandidateContext`、`CommitContext`
- `Candidate`、`CivilTime`、`Config`、`Engine`、`Key`、`UserDict`
- `Fuzzy`、`Script`、`Mode`、`Event`
- `SDK_VERSION`

`CandidateContext` 提供:

- `raw`:原始击键(双拼键、辅助码分隔符等)。
- `query`:方案展开后的规范全拼;全拼方案与 `raw` 相同。
- `scheme`:当前方案 id。
- `context`、`host`、`now`:左邻文本、宿主 exe、本地时间。
- `previous`、`previous2`:上屏词序,供联想使用。
- `user`:当前用户词库只读视图。

## 最小模块

```rust
use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};

struct Demo;

impl Module for Demo {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "demo",
            name: "演示",
            version: "1.0",
            capabilities: &[Capability::Candidates],
        }
    }

    fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        if ctx.query == "demo" {
            let mut cand = Candidate::whole(ctx.raw.len(), "演示", 1.0);
            cand.hint = Some("扩展".into());
            cands.insert(0, cand);
        }
    }
}
```

## 最小方案

```rust
use lueying_ext::{Expansion, Scheme, SchemeManifest};

struct Uppercase;

impl Scheme for Uppercase {
    fn manifest(&self) -> SchemeManifest {
        SchemeManifest { id: "upper", name: "大写方案", version: "1.0" }
    }

    fn expand(&self, raw: &str) -> Expansion {
        Expansion::identity(&raw.to_ascii_lowercase())
    }
}
```

实际方案应像 `lueying-features::shuangpin` 一样填写 `raw_at`、`raw_to_query`、`segments` 与 `key`,否则候选区间和学习键会落在错误的击键上。

## 注册与生命周期

扩展随宿主编译,由 `lueying-server` 在启动时注册:

- 输入方案:`lueying_features::scheme(SchemeId, aux)` 构造,`Engine::with_scheme` 注入。
- 候选模块:`lueying_features::registry(...)` 组合,`Engine::with_modules` 注入。
- 设置变更后,常驻服务调用 `Engine::set_scheme` / `set_config`,并让所有现有会话重算。

运行期不加载用户目录里的 DLL。输入法进程运行在宿主应用内,加载未签名代码的收益低于风险;需要动态分发时,应先做签名、版本与权限模型。

## 版本与兼容

`Manifest.version` 与 `SchemeManifest.version` 由扩展自行填写;`SDK_VERSION` 是当前 SDK crate 版本。候选结构新增字段属于 SDK 变更,扩展应跟随工作区版本重新编译。

## 测试

扩展以 E2E 为主:复杂链路与跨方案行为用 `crates/e2e/src/extensions.rs` 验证;清单与补丁的纯逻辑边界另有单元测试(`crates/extjs/src/*_tests.rs`),补回归网密度:

- 从按键序列走到候选、上屏、学习与跨方案召回。
- 覆盖错误路径:辅码无匹配、短语坏行、方案切换后的组合清理。
- 产出可复查的 Markdown 报告,由 CI artifact 保留。

## 开发回路

`lueying ext` 命令行覆盖扩展的建包、打包、安装、重载、日志与运行指标;
`lueying ext replay` 在 Linux 无头环境回放按键脚本(按键序列进,候选列表
出),用来验证拼音行为与触发词,输出确定性、可直接当回归用例。面向
coding agent 的完整流程见 [agent-guide.md](agent-guide.md)。

## 外部启动器

扩展命令可以从掠影之外触发:`lueying ext run <命令 id>` 与动作候选、
热键走同一条执行路径,PowerToys 命令面板用「书签」包住这条命令就能
唤起。接法与取舍见 [powertoys.md](powertoys.md)。

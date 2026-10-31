//! 可选功能模块的契约与注册表。
//!
//! 核心引擎只负责默认全拼/简拼;符号、快捷输入、表情、英文混输、简繁转换、
//! 自定义短语与下一词联想,都是同一契约的功能模块。双拼、辅助码会改变
//! 音节切分,以引擎级方案接口扩展。模块由宿主在启动时组合进 [`Modules`],
//! 引擎按注册顺序调用,默认注册表为空,现有输入行为与性能不变。
//!
//! 参考 pi / deepseek harness 的模块化思路:核心定义稳定契约与生命周期,宿主管
//! 组合与顺序,功能模块只依赖核心契约。这里使用编译期注册而不是运行期加载
//! DLL,避免把未签名代码加载进输入法进程。

use std::sync::Arc;

use crate::{Candidate, CivilTime, Key, Mode};

/// 模块声明。宿主用它展示已接入的能力、诊断冲突或按开关筛选。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    /// 模块参与的阶段。注册表只调用声明过的阶段,漏声明等于该阶段不生效。
    pub capabilities: &'static [Capability],
    /// 触发前缀(如 `v`、`u`):声明了 [`Capability::Key`] 的模块,只有组合
    /// 以其中之一开头才会被咨询键级钩子。不参与按键的模块留空。
    pub triggers: &'static [&'static str],
}

impl Manifest {
    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// 在核心候选生成后插入、删除、重排或改写候选。
    Candidates,
    /// 上屏前转换文本;偏爱在候选阶段完成转换的模块不需要它。
    Commit,
    /// 每键拦截:触发会话里把标点/数字/字母当输入组合。只给清单声明了
    /// [`Manifest::triggers`] 的模块进这条热路径。
    Key,
}

/// 键级钩子的只读上下文。
pub struct KeyContext<'a> {
    /// 当前组合的原始输入;组合刚开始时是触发前缀本身。
    pub raw: &'a str,
}

/// 候选阶段的只读上下文。
pub struct CandidateContext<'a> {
    /// 当前组合的原始输入(小写字母、隔音符与方案符号键)。
    pub raw: &'a str,
    /// 方案展开后的规范全拼;全拼方案与 `raw` 相同,双拼为展开结果。
    pub query: &'a str,
    /// 当前方案 id(如 `full`、`xiaohe`)。
    pub scheme: &'a str,
    /// 当前组合是否命中快捷触发串(`rq`/`sj`/`xq`、符号码、u/v 编码)。
    /// 依赖字母回退的模块(如英文混输)据此让位。
    pub quick_trigger: bool,
    /// 左邻文本,最多 [`crate::CONTEXT_KEEP`] 字。
    pub context: &'a str,
    /// 宿主 exe 名(小写);空表示未知。
    pub host: &'a str,
    /// 宿主注入的本地时间。
    pub now: CivilTime,
    /// 上一个上屏词;用于下一词联想。
    pub previous: Option<&'a str>,
    /// 上上个上屏词;用于三词窗口。
    pub previous2: Option<&'a str>,
    /// 当前用户词库只读视图;模块可据此学习、联想或做个性化排序。
    pub user: &'a crate::UserDict,
}

/// 上屏阶段的只读上下文。
pub struct CommitContext<'a> {
    pub host: &'a str,
    pub mode: Mode,
}

/// 一个可选功能模块。默认实现不改变任何行为,模块只覆盖自己关心的阶段。
pub trait Module: Send + Sync {
    fn manifest(&self) -> Manifest;

    /// 核心候选生成后按注册顺序调用。模块可插入、删除或重排候选。
    fn candidates(&self, _ctx: &CandidateContext<'_>, _cands: &mut Vec<Candidate>) {}

    /// 上屏事件离开引擎前调用。只在最终事件流上执行一次,不会与候选阶段叠加
    /// 转换;需要让学习与上下文看到同一种写法的模块应在候选阶段完成改写。
    fn commit(&self, _ctx: &CommitContext<'_>, _text: &mut String) {}

    /// 触发会话里的按键咨询:返回 true 表示该键属于本模块的输入串,引擎
    /// 按组合字符处理,不当标点、翻页或候选序号。只在组合以 [`Manifest::triggers`]
    /// 中的前缀开头时被调用,未命中触发串的按键不会进入本方法。
    fn key(&self, _ctx: &KeyContext<'_>, _key: &Key) -> bool {
        false
    }

    /// 当前串是否处于本模块的触发会话。引擎在查询构建前问一次,用于跳过
    /// 错音纠正等工作;同样以触发前缀为闸门。
    fn trigger(&self, _raw: &str) -> bool {
        false
    }
}

#[derive(Clone)]
struct Entry {
    manifest: Manifest,
    module: Arc<dyn Module>,
}

/// 按注册顺序执行的功能模块集合。克隆只复制 `Arc`,会话之间共享同一组模块。
#[derive(Clone, Default)]
pub struct Modules {
    entries: Vec<Entry>,
}

impl Modules {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(mut self, module: Arc<dyn Module>) -> Self {
        let manifest = module.manifest();
        self.entries.push(Entry { manifest, module });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn manifests(&self) -> impl Iterator<Item = Manifest> + '_ {
        self.entries.iter().map(|entry| entry.manifest)
    }

    /// 按注册顺序遍历模块本体;宿主重组注册表时用。
    pub fn modules(&self) -> impl Iterator<Item = Arc<dyn Module>> + '_ {
        self.entries.iter().map(|entry| Arc::clone(&entry.module))
    }

    /// 触发会话里的按键认领:任一声明了 [`Capability::Key`] 且触发前缀命中
    /// 的模块认领即成立。未命中前缀时不进入任何模块,热路径零开销。
    pub(crate) fn claims_key(&self, raw: &str, key: &Key) -> bool {
        for entry in &self.entries {
            if !entry.manifest.has(Capability::Key)
                || !entry.manifest.triggers.iter().any(|t| raw.starts_with(t))
            {
                continue;
            }
            let ctx = KeyContext { raw };
            if entry.module.key(&ctx, key) {
                return true;
            }
        }
        false
    }

    /// 当前串是否处于任一模块的触发会话;查询层据此跳过错音纠正。
    pub(crate) fn raw_trigger(&self, raw: &str) -> bool {
        for entry in &self.entries {
            if !entry.manifest.has(Capability::Key)
                || !entry.manifest.triggers.iter().any(|t| raw.starts_with(t))
            {
                continue;
            }
            if entry.module.trigger(raw) {
                return true;
            }
        }
        false
    }

    pub(crate) fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        for entry in &self.entries {
            if entry.manifest.has(Capability::Candidates) {
                entry.module.candidates(ctx, cands);
            }
        }
    }

    pub(crate) fn commit(&self, ctx: &CommitContext<'_>, text: &mut String) {
        for entry in &self.entries {
            if entry.manifest.has(Capability::Commit) {
                entry.module.commit(ctx, text);
            }
        }
    }
}

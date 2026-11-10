//! 本地神经重排:把引擎候选交给字符级语言模型重新排序。
//!
//! 采用 metasequoia 参考实现的门控:词典整键精确命中不覆盖、只比较回答整个
//! 按键的候选、最多九条。8M 走同步按键路径,25M 只在下文 [`debounce`] 的停顿
//! 槽位运行;两者共用同一套 [`Fact`] 契约。

pub mod lm;

pub mod debounce;
pub mod metrics;

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;

use std::sync::Arc;
use std::time::Instant;

pub use lm::ModelError;
use lm::{CandidateFacts, Reranker, SentenceModel};

pub use metrics::{LatencyMetrics, LatencySnapshot};

/// 加载后的模型。权重只读,克隆只增加引用计数,多个会话共享一份。
#[derive(Clone)]
pub struct Model {
    inner: Arc<SentenceModel>,
}

impl Model {
    pub fn load(bytes: &[u8]) -> Result<Self, ModelError> {
        Ok(Self {
            inner: Arc::new(SentenceModel::load(bytes)?),
        })
    }

    /// 加载上游按键重排模型:`.ort` 图加同目录的 `model.json`、`vocab.json`。
    pub fn load_keystroke(
        ort: &[u8],
        manifest: &str,
        vocabulary: &str,
    ) -> Result<Self, ModelError> {
        Ok(Self {
            inner: Arc::new(SentenceModel::load_keystroke(ort, manifest, vocabulary)?),
        })
    }

    /// 从目录加载一组按键重排模型文件,`stem` 形如 `rerank-8m`。
    pub fn load_keystroke_dir(dir: &std::path::Path, stem: &str) -> Result<Self, String> {
        let read = |suffix: &str| {
            let path = dir.join(format!("{stem}{suffix}"));
            std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
        };
        let ort = read(".ort")?;
        let manifest = read(".model.json")?;
        let vocabulary = read(".vocab.json")?;
        let manifest =
            String::from_utf8(manifest).map_err(|error| format!("{stem}.model.json: {error}"))?;
        let vocabulary =
            String::from_utf8(vocabulary).map_err(|error| format!("{stem}.vocab.json: {error}"))?;
        Self::load_keystroke(&ort, &manifest, &vocabulary)
            .map_err(|error| format!("{stem}: {error}"))
    }

    /// 模型能接受的上下文窗口(字符数)。
    pub fn context_length(&self) -> usize {
        self.inner.context_length()
    }

    /// 权重许可(SPDX)。空串表示文件没有声明。
    pub fn license(&self) -> &str {
        self.inner.license()
    }

    /// 训练语料署名。
    pub fn attribution(&self) -> &str {
        self.inner.attribution()
    }
}

/// 一条候选相对当前按键的事实。两者都只有引擎知道,不能从候选文本推断。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fact {
    /// 候选覆盖整个组合输入,而不是前缀词或联想补全。
    pub answers_key: bool,
    /// 词典对用户实际键入的整键给出的精确命中,没有上文时值得让模型让位。
    pub trusted_dictionary_hit: bool,
    /// 引擎总分(语言分减读法代价,含个人上下文)。
    pub score: f32,
    /// 这种读法的代价(nat)。模型只看文字,看不到用户实际打的键。
    pub channel: f32,
    /// 引擎总分里来自用户学习的部分。
    pub personal: f32,
    /// 用户在这个键下稳定选过这个词,任何模型都不改。
    pub locked: bool,
    /// 经模糊音或手误改动过读音。
    pub edited: bool,
}

/// 有上文时的决策目标:模型整段对数概率 + 引擎语言分 × [`ENGINE_WEIGHT`]
/// − 读法代价 × [`CHANNEL_WEIGHT`] + 用户学习分 × [`PERSONAL_WEIGHT`],引擎首位再加
/// [`LEADER_BONUS`]。前三个系数在 v4 开发集有上文的样本上拟合,两半交叉验证差异在 0.5 点内。
/// 基准里学习是关着的,学习分恒为零,这些数字不受 [`PERSONAL_WEIGHT`] 影响。
///
/// 有上文时这个目标在所有打法上都胜过词频(词级全拼 79 → 83,分段全拼 48 → 76);
/// 没有上文时模型只能比字频,孤立词上反而低十几点,仍走词频优先的旧门控。
const CHANNEL_WEIGHT: f32 = 1.5;
const ENGINE_WEIGHT: f32 = 0.1;
const PERSONAL_WEIGHT: f32 = 1.0;
const LEADER_BONUS: f32 = 0.5;
/// 一次比较的候选数与总字数上限。短段的正确字常排在十几名开外,要看得足够深;
/// 总字数上限等于整句时九条候选的工作量,不比原来贵。
const COMPARED_MAX: usize = 32;
const COMPARED_CHARS: usize = 144;
/// 没有上文时,这么短的首位算短句,要领先 [`SHORT_MARGIN`] 才换。
const SHORT_CHARS: usize = 4;
const SHORT_MARGIN: f32 = 1.0;

/// 8M 同步重排的最小领先幅度(每字符平均对数概率)。
///
/// 8M 比 25M 小,分数更容易被噪声拉平;共享语料上它在小分差时改首选,
/// top-1 反而低于引擎原序。只让模型明确领先的候选越过首位,把即时排序
/// 留给词频与语法模型,停顿后再由 25M 做更细的判断。
pub const SYNC_PROMOTE_MARGIN: f32 = 0.05;

/// 一个会话独享的推理状态:跨按键前缀缓存和候选续算都放在这里。
pub struct Rerank {
    inner: Reranker,
    metrics: Arc<LatencyMetrics>,
}

impl Rerank {
    pub fn new(model: &Model) -> Self {
        Self::with_metrics(model, Arc::new(LatencyMetrics::default()))
    }

    /// 会话共享一份延迟直方图;8M 与 25M 各传各的。
    pub fn with_metrics(model: &Model, metrics: Arc<LatencyMetrics>) -> Self {
        Self {
            inner: Reranker::new(Arc::clone(&model.inner)),
            metrics,
        }
    }

    /// 当前会话上报的延迟分布。
    pub fn metrics(&self) -> Arc<LatencyMetrics> {
        Arc::clone(&self.metrics)
    }

    /// 返回模型建议移到首位的候选下标;`None` 表示保持引擎顺序。
    ///
    /// `texts` 与 `facts` 必须一一对应,长度不一致时放弃决策。
    pub fn decide(&mut self, context: &str, texts: &[&str], facts: &[Fact]) -> Option<usize> {
        let started = Instant::now();
        let result = self.decide_inner(context, texts, facts);
        self.metrics.record(started.elapsed());
        result
    }

    /// 返回模型建议移到首位的候选下标;`None` 表示保持引擎顺序。
    /// 指定最小领先幅度(`min_margin`),传入 `0.0` 即为原始 `best_where`。
    pub fn decide_with_margin(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        min_margin: f32,
    ) -> Option<usize> {
        let started = Instant::now();
        let result = self.decide_inner_with_margin(context, texts, facts, min_margin);
        self.metrics.record(started.elapsed());
        result
    }

    fn decide_inner(&mut self, context: &str, texts: &[&str], facts: &[Fact]) -> Option<usize> {
        if !context.is_empty() {
            return self.decide_in_context(context, texts, facts);
        }
        self.decide_inner_with_margin(context, texts, &same_reading(facts), SYNC_PROMOTE_MARGIN)
    }

    /// 有上文时的决策,见 [`CHANNEL_WEIGHT`]。
    fn decide_in_context(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
    ) -> Option<usize> {
        if texts.len() != facts.len() {
            return None;
        }
        let leader = facts.first()?;
        if leader.locked || !leader.answers_key {
            return None;
        }
        let mut budget = COMPARED_CHARS;
        let mut compared: Vec<usize> = Vec::with_capacity(COMPARED_MAX);
        for (index, (text, fact)) in texts.iter().zip(facts).enumerate() {
            if !fact.answers_key {
                continue;
            }
            let chars = text.chars().count();
            if compared.len() == COMPARED_MAX || (chars > budget && compared.len() >= 2) {
                break;
            }
            budget = budget.saturating_sub(chars);
            compared.push(index);
        }
        if compared.len() < 2 {
            return None;
        }
        let batch: Vec<&str> = compared.iter().map(|&index| texts[index]).collect();
        let means = self.inner.log_probabilities(context, &batch);
        if means.len() != compared.len() {
            return None;
        }
        let objective = |at: usize| {
            let fact = &facts[compared[at]];
            let total = means[at] * batch[at].chars().count() as f32;
            let leading = if at == 0 { LEADER_BONUS } else { 0.0 };
            total + ENGINE_WEIGHT * (fact.score + fact.channel - fact.personal)
                - CHANNEL_WEIGHT * fact.channel
                + PERSONAL_WEIGHT * fact.personal
                + leading
        };
        let best = (0..compared.len()).max_by(|&a, &b| objective(a).total_cmp(&objective(b)))?;
        (best != 0).then_some(compared[best])
    }

    fn decide_inner_with_margin(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        min_margin: f32,
    ) -> Option<usize> {
        if texts.len() != facts.len() {
            return None;
        }
        self.inner.best_where_margin(
            context,
            texts,
            |index| CandidateFacts {
                answers_key: facts[index].answers_key,
                trusted_dictionary_hit: facts[index].trusted_dictionary_hit,
            },
            min_margin,
        )
    }

    /// 停顿重排。有上文时与 8M 用同一个目标,只是模型更大;没有上文时模型只能比字频,
    /// 精确首位不动,也不让给改动过的读法(开发呢 → 开发了),短句要明显领先
    /// (市场吗 → 市场买)才换。调用方只应在静默停顿、未翻页、未移动高亮且组合仍活跃时使用。
    pub fn decide_settled(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
    ) -> Option<usize> {
        let started = Instant::now();
        let result = self.decide_settled_inner(context, texts, facts);
        self.metrics.record(started.elapsed());
        result
    }

    fn decide_settled_inner(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
    ) -> Option<usize> {
        if !context.is_empty() {
            return self.decide_in_context(context, texts, facts);
        }
        let leader = facts.first()?;
        if leader.trusted_dictionary_hit || !leader.answers_key {
            return None;
        }
        let short = texts
            .first()
            .is_some_and(|text| text.chars().count() <= SHORT_CHARS);
        let margin = if short { SHORT_MARGIN } else { 0.0 };
        self.decide_inner_with_margin(context, texts, &same_reading(facts), margin)
    }

    /// 每条候选的每字符平均对数概率,按输入顺序返回。评测与诊断用。
    pub fn scores(&mut self, context: &str, texts: &[&str]) -> Vec<f32> {
        self.inner.log_probabilities(context, texts)
    }
}

/// 没有上文时模型只能比字频,不凭它把没改动过的首位换成改动过的读法。
fn same_reading(facts: &[Fact]) -> Vec<Fact> {
    let edited = facts.first().is_some_and(|fact| fact.edited);
    facts
        .iter()
        .map(|fact| Fact {
            answers_key: fact.answers_key && (edited || !fact.edited),
            ..*fact
        })
        .collect()
}

pub mod settle {
    /// 停顿结果能否落地:输入没变、还在一页的首行、组合仍在。
    pub fn result_is_current(
        engine_revision: u64,
        result_revision: u64,
        page: usize,
        highlight: usize,
        composing: bool,
    ) -> bool {
        engine_revision == result_revision && page == 0 && highlight == 0 && composing
    }
}

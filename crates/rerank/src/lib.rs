//! 本地神经重排:把引擎候选交给字符级语言模型重新排序。
//!
//! 采用 metasequoia 参考实现的门控:词典整键精确命中不覆盖、只比较回答整个
//! 按键的候选、最多九条。4M 走同步按键路径,25M 只在下文 [`debounce`] 的停顿
//! 槽位运行;两者共用同一套 [`Fact`] 契约。

pub mod debounce;
pub mod metrics;

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod metrics_tests;

use std::sync::Arc;
use std::time::Instant;

pub use chinese_ime_lm::ModelError;
use chinese_ime_lm::{CandidateFacts, Reranker, SentenceModel};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fact {
    /// 候选覆盖整个组合输入,而不是前缀词或联想补全。
    pub answers_key: bool,
    /// 词典对用户实际键入的整键给出的精确命中,值得让模型让位。
    pub trusted_dictionary_hit: bool,
}

/// 停顿重排越过精确首位的门槛(每字符平均对数概率)。
/// 只让上下文明确的模糊候选小幅领先时才改首选,快速连打时仍保持稳定。
pub const SETTLE_OVERRIDE_MARGIN: f32 = 0.02;

/// 4M 同步重排的最小领先幅度(每字符平均对数概率)。
///
/// 4M 比 25M 小,分数更容易被噪声拉平;共享语料上它在小分差时改首选,
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

    /// 会话共享一份延迟直方图;4M 与 25M 各传各的。
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

    fn decide_inner(&mut self, context: &str, texts: &[&str], facts: &[Fact]) -> Option<usize> {
        if texts.len() != facts.len() {
            return None;
        }
        self.inner
            .best_where_margin(
                context,
                texts,
                |index| CandidateFacts {
                    answers_key: facts[index].answers_key,
                    trusted_dictionary_hit: facts[index].trusted_dictionary_hit,
                },
                SYNC_PROMOTE_MARGIN,
            )
    }

    /// 停顿重排:精确首位在上下文明确时给模糊候选让位。
    ///
    /// `decide` 在首条是可信精确命中时直接放弃;这里补一条受控通道:只有
    /// 后置的模糊候选在 25M 模型下领先超过 [`SETTLE_OVERRIDE_MARGIN`] 才改。
    /// 调用方只应在静默停顿、未翻页、未移动高亮且组合仍活跃时使用。
    pub fn decide_settled(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        fuzzy: &[bool],
    ) -> Option<usize> {
        let started = Instant::now();
        let result = self.decide_settled_inner(context, texts, facts, fuzzy);
        self.metrics.record(started.elapsed());
        result
    }

    fn decide_settled_inner(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        fuzzy: &[bool],
    ) -> Option<usize> {
        if texts.len() != facts.len() || texts.len() != fuzzy.len() {
            return None;
        }
        let leader = facts.first()?;
        if !leader.trusted_dictionary_hit || !leader.answers_key {
            return self.decide_inner(context, texts, facts);
        }
        if context.is_empty() {
            return None;
        }
        let scores = self.scores(context, texts);
        if scores.len() != texts.len() {
            return None;
        }
        let mut best = 0;
        for (index, &score) in scores.iter().enumerate().take(9) {
            if facts[index].answers_key && fuzzy[index] && score > scores[best] {
                best = index;
            }
        }
        (best != 0 && scores[best] - scores[0] >= SETTLE_OVERRIDE_MARGIN).then_some(best)
    }

    /// 每条候选的每字符平均对数概率,按输入顺序返回。评测与诊断用。
    pub fn scores(&mut self, context: &str, texts: &[&str]) -> Vec<f32> {
        self.inner.log_probabilities(context, texts)
    }
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

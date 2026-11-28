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

    /// Compiles the existing ORT execution weights to a self-contained safetensors file.
    pub fn pack_keystroke_dir(
        dir: &std::path::Path,
        stem: &str,
        out: &mut impl std::io::Write,
    ) -> Result<(), String> {
        let model = Self::load_keystroke_dir(dir, stem)?;
        let vocab = std::fs::read_to_string(dir.join(format!("{stem}.vocab.json")))
            .map_err(|error| error.to_string())?;
        #[derive(serde::Deserialize)]
        struct Vocabulary {
            tokens: Vec<String>,
        }
        let vocab: Vocabulary = serde_json::from_str(&vocab).map_err(|error| error.to_string())?;
        model
            .inner
            .pack(&vocab.tokens, out)
            .map_err(|error| error.to_string())
    }

    /// Maps an immutable, locally prepacked keystroke model directly into inference.
    #[cfg(windows)]
    pub fn load_packed_file(path: &std::path::Path) -> Result<Self, String> {
        use lm::weights::{Mapping, PACKED_FORMAT, Weights};
        let map = Mapping::open(path).map_err(|error| error.to_string())?;
        let weights = Weights::parse_mapped(&map).map_err(|error| error.to_string())?;
        if weights.metadata("format").ok() != Some(PACKED_FORMAT) {
            return Err("not a Lueying prepacked keystroke model".into());
        }
        let model = SentenceModel::load_weights(weights).map_err(|error| error.to_string())?;
        Mapping::warm(&map);
        Ok(Self {
            inner: Arc::new(model),
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
    /// 候选按可自动比较的读法覆盖整个组合输入,而非前缀、补全或被更明确读法支配的切分。
    pub answers_key: bool,
    /// 词典对用户实际键入的整键给出的精确命中,没有上文时值得让模型让位。
    pub trusted_dictionary_hit: bool,
    /// 引擎总分(语言分减读法代价,含个人上下文)。
    pub score: f32,
    /// 这种读法的代价(nat)。模型只看文字,看不到用户实际打的键。
    pub channel: f32,
    /// 引擎总分里来自用户学习的部分。
    pub personal: f32,
    /// 用户在这个键下稳定选过这个词,或词典确认它能与刚上屏的词接成整词;任何模型都不改。
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
/// 停顿重排在静默期跑,延迟预算宽得多:比选窗口可以放深三倍,够到引擎序里
/// 几十名开外的正确文本(深度救援在 32 条窗口里物理上不可达)。
const SETTLE_COMPARED_MAX: usize = 96;
const SETTLE_COMPARED_CHARS: usize = 384;
const SETTLE_BUDGET: (usize, usize) = (SETTLE_COMPARED_MAX, SETTLE_COMPARED_CHARS);
/// 预筛检索池深度:停顿决策先用小模型给引擎序前这么多条整键候选打分,
/// 比选窗口只装它认为最有希望的前 [`SETTLE_COMPARED_MAX`] 名。便宜模型
/// 做检索、贵模型做裁决——引擎序 96~384 名的正确文本由此够得着,
/// 而大模型的比选工作量不变。
const PREFILTER_POOL: usize = 384;
/// 空上文的停顿窗口:没有上文时预筛层无从谈起(小模型同样只能比字频),
/// 但融合目标下更深的引擎序窗仍能再救几条,单独给一份更深的预算。
const EMPTY_SETTLE_BUDGET: (usize, usize) = (192, 768);

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

/// The scored candidate slots and the score gap between the first two.
#[derive(Default)]
pub struct ContextRank {
    pub order: Vec<usize>,
    pub gap: Option<f32>,
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

    /// Forget the previous session without reallocating the inference workspace.
    pub fn clear_history(&mut self) {
        self.inner.clear_history();
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
            return self.decide_in_context(
                context,
                texts,
                facts,
                None,
                (COMPARED_MAX, COMPARED_CHARS),
                None,
            );
        }
        self.decide_inner_with_margin(context, texts, &same_reading(facts), SYNC_PROMOTE_MARGIN)
    }

    /// 有上文时的决策,见 [`CHANNEL_WEIGHT`]。`budget` 是比选窗口的
    /// (条数, 总字数) 上限;每键同步路径用紧预算,停顿路径用 [`SETTLE_COMPARED_MAX`]。
    /// `prefilter` 给一个小模型检索器:窗口内容改由它从更深的候选池里挑,
    /// 见 [`prefiltered_order`];`None` 退回引擎序窗口。
    fn decide_in_context(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        left: Option<&str>,
        budget: (usize, usize),
        prefilter: Option<&mut Rerank>,
    ) -> Option<usize> {
        let (indices, scores) =
            self.context_scores(context, texts, facts, false, budget, prefilter)?;
        let best = (0..indices.len()).max_by(|&a, &b| scores[a].total_cmp(&scores[b]))?;
        let winner = indices[best];
        (winner != 0 && !self.local_disagrees(left, texts, facts, winner)).then_some(winner)
    }

    /// Reorder the scored slots using the same batch and objective as head-only promotion.
    /// Returns their original candidate indices in descending score order.
    pub fn rank_in_context(&mut self, context: &str, texts: &[&str], facts: &[Fact]) -> Vec<usize> {
        self.rank_in_context_with_gap(context, texts, facts).order
    }

    /// Expose the top gap from the same pass, without running the model again.
    /// The gap is absent when local adjacency evidence puts a candidate other than
    /// the model's top-scoring one first.
    pub fn rank_in_context_with_gap(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
    ) -> ContextRank {
        self.rank_with_local_context(context, texts, facts, None)
    }

    /// 有上文时重排;知道相邻上屏词时,用连接与断句的对照检验首选替换。
    /// The gap is absent when the returned order's top is not the model score order.
    pub fn rank_with_local_context(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        left: Option<&str>,
    ) -> ContextRank {
        self.rank_with_local_context_prefiltered(context, texts, facts, left, None)
    }

    /// `rank_with_local_context` 的预筛版:比选窗口内容由小模型从
    /// `PREFILTER_POOL` 深的候选池里挑,见 `prefiltered_order`。
    pub fn rank_with_local_context_prefiltered(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        left: Option<&str>,
        prefilter: Option<&mut Rerank>,
    ) -> ContextRank {
        if context.is_empty() {
            return ContextRank::default();
        }
        let started = Instant::now();
        let result = self
            .context_scores(context, texts, facts, true, SETTLE_BUDGET, prefilter)
            .map_or_else(ContextRank::default, |(indices, scores)| {
                let mut order: Vec<_> = indices.into_iter().zip(scores).collect();
                order.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
                let gap = if self.local_disagrees(left, texts, facts, order[0].0) {
                    // 句末/独立位置更适合挑战者,不要拿它替换直连证据更强的首位。
                    let head = order
                        .iter()
                        .position(|&(index, _)| index == 0)
                        .expect("leader was scored");
                    order[..=head].rotate_right(1);
                    None
                } else {
                    order.get(1).map(|second| order[0].1 - second.1)
                };
                ContextRank {
                    order: order.into_iter().map(|(index, _)| index).collect(),
                    gap,
                }
            });
        self.metrics.record(started.elapsed());
        result
    }

    fn local_disagrees(
        &self,
        left: Option<&str>,
        texts: &[&str],
        facts: &[Fact],
        winner: usize,
    ) -> bool {
        winner != 0
            && facts[0].trusted_dictionary_hit
            && !facts[0].edited
            && (facts[winner].trusted_dictionary_hit || facts[winner].edited)
            && left.is_some_and(|left| {
                let (leader, challenger) = (texts[0], texts[winner]);
                self.inner
                    .adjacency_contradicts(left, leader, challenger)
                    // 跨读音纠错已经由完整上文打分;仅短窗口反对,不足以否决。
                    && (!facts[winner].edited
                        || self
                            .inner
                            .full_context_confirms_veto(left, leader, challenger))
            })
    }

    fn context_scores(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        keep_locked: bool,
        (max_cand, max_chars): (usize, usize),
        prefilter: Option<&mut Rerank>,
    ) -> Option<(Vec<usize>, Vec<f32>)> {
        if texts.len() != facts.len() {
            return None;
        }
        let leader = facts.first()?;
        if leader.locked || !leader.answers_key {
            return None;
        }
        let mut budget = max_chars;
        let mut compared: Vec<usize> = Vec::with_capacity(max_cand);
        let mut indices: Box<dyn Iterator<Item = usize>> = match prefilter {
            Some(filter) => Box::new(prefiltered_order(context, texts, facts, filter).into_iter()),
            None => Box::new(0..texts.len()),
        };
        for index in indices.by_ref() {
            let fact = &facts[index];
            if !fact.answers_key || (keep_locked && fact.locked) {
                continue;
            }
            let chars = texts[index].chars().count();
            if compared.len() == max_cand || (chars > budget && compared.len() >= 2) {
                break;
            }
            budget = budget.saturating_sub(chars);
            compared.push(index);
        }
        if compared.len() < 2 {
            return None;
        }
        let batch: Vec<&str> = compared.iter().map(|&index| texts[index]).collect();
        let mut scores = self.inner.log_probabilities(context, &batch);
        if scores.len() != compared.len() {
            return None;
        }
        for (at, score) in scores.iter_mut().enumerate() {
            let fact = &facts[compared[at]];
            let total = *score * batch[at].chars().count() as f32;
            let leading = if at == 0 { LEADER_BONUS } else { 0.0 };
            *score = total + ENGINE_WEIGHT * (fact.score + fact.channel - fact.personal)
                - CHANNEL_WEIGHT * fact.channel
                + PERSONAL_WEIGHT * fact.personal
                + leading;
        }
        Some((compared, scores))
    }

    /// 比选窗口内各候选的混合目标得分,按降序返回 `(候选下标, 得分)`。
    /// 诊断用;不改变 `decide` 的门控语义(local_disagrees 等在调用方之外)。
    #[doc(hidden)]
    pub fn context_scored(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
    ) -> Vec<(usize, f32)> {
        let Some((indices, scores)) =
            self.context_scores(context, texts, facts, false, SETTLE_BUDGET, None)
        else {
            return Vec::new();
        };
        let mut order: Vec<_> = indices.into_iter().zip(scores).collect();
        order.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        order
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

    /// 停顿重排。有上文时与 8M 用同一个目标,只是模型更大;没有上文时用同一个
    /// 融合目标裁决,精确可信首位不动,也不让给改动过的读法(开发呢 → 开发了)。
    /// 调用方只应在静默停顿、未翻页、未移动高亮且组合仍活跃时使用。
    pub fn decide_settled(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
    ) -> Option<usize> {
        self.decide_settled_with_local_context(context, texts, facts, None)
    }

    /// 带相邻上屏词的停顿决策。
    pub fn decide_settled_with_local_context(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        left: Option<&str>,
    ) -> Option<usize> {
        self.decide_settled_prefiltered(context, texts, facts, left, None)
    }

    /// 停顿决策的预筛版:有上文时比选窗口由小模型从 `PREFILTER_POOL` 深的
    /// 候选池里挑(便宜模型检索、贵模型裁决);无上文时预筛不起作用。
    pub fn decide_settled_prefiltered(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        left: Option<&str>,
        prefilter: Option<&mut Rerank>,
    ) -> Option<usize> {
        let started = Instant::now();
        let result = self.decide_settled_inner(context, texts, facts, left, prefilter);
        self.metrics.record(started.elapsed());
        result
    }

    fn decide_settled_inner(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: &[Fact],
        left: Option<&str>,
        prefilter: Option<&mut Rerank>,
    ) -> Option<usize> {
        if !context.is_empty() {
            return self.decide_in_context(context, texts, facts, left, SETTLE_BUDGET, prefilter);
        }
        let leader = facts.first()?;
        if leader.trusted_dictionary_hit || !leader.answers_key {
            return None;
        }
        // 空上文用与有上文完全相同的融合裁决:模型整段对数概率 + 引擎分 − 读法
        // 代价,首位照常拿 LEADER_BONUS。same_reading 仍然把「未改动首位换成
        // 改动读法」(开发呢→开发了)挡在比选集合之外;可信整键命中依然不动。
        self.decide_in_context(
            context,
            texts,
            &same_reading(facts),
            left,
            EMPTY_SETTLE_BUDGET,
            None,
        )
    }

    /// 每条候选的每字符平均对数概率,按输入顺序返回。评测与诊断用。
    pub fn scores(&mut self, context: &str, texts: &[&str]) -> Vec<f32> {
        self.inner.log_probabilities(context, texts)
    }
}

/// 预筛窗口序:首位候选固定领头,其余整键候选按小模型的混合分
/// (整段对数概率 + 引擎分 − 读法代价,与裁决目标同源、不含首位加成)
/// 降序排列。小模型只决定「谁值得进窗」,窗口内的先后仍由裁决模型说了算。
fn prefiltered_order(
    context: &str,
    texts: &[&str],
    facts: &[Fact],
    filter: &mut Rerank,
) -> Vec<usize> {
    let indices: Vec<usize> = (0..texts.len())
        .filter(|&index| facts[index].answers_key)
        .take(PREFILTER_POOL)
        .collect();
    if indices.len() <= 1 {
        return indices;
    }
    let batch: Vec<&str> = indices.iter().map(|&index| texts[index]).collect();
    let scores = filter.scores(context, &batch);
    if scores.len() != indices.len() {
        return indices;
    }
    let mut keyed: Vec<(usize, f32)> = indices
        .iter()
        .copied()
        .zip(scores)
        .map(|(index, score)| {
            let fact = &facts[index];
            let total = score * texts[index].chars().count() as f32;
            (
                index,
                total + ENGINE_WEIGHT * (fact.score + fact.channel - fact.personal)
                    - CHANNEL_WEIGHT * fact.channel
                    + PERSONAL_WEIGHT * fact.personal,
            )
        })
        .collect();
    keyed[1..].sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
    keyed.into_iter().map(|(index, _)| index).collect()
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

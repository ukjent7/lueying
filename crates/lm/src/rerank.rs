//! 候选重排:前缀缓存、增量续跑与比较范围判定。
//!
//! `SentenceModel` 留在 crate 根,这里只放重排状态机;字段对测试保持
//! crate 内可见,因为 `bounded_encoding_tests` 会用同一结构对照旧实现。

use super::{comparable, CandidateFacts, Prefix, SentenceModel, TailRun, DICTIONARY_SOURCES};
use std::sync::Arc;

/// A model plus the prefix it last ran, so that typing does not re-run the committed text.
///
/// The prefix is the expensive half of a short decision — nineteen characters of context cost about
/// as much as the candidates themselves — and it only changes when something is committed, not on
/// every keystroke. Holding it across keystrokes is what puts the decision inside a frame.
pub struct Reranker {
    /// Shared, because the weights are read-only and a host opens a session per focus change.
    /// Only the cached prefix belongs to one session.
    pub(crate) model: Arc<SentenceModel>,
    pub(crate) cached: Option<(Vec<u32>, Prefix<'static>)>,
    /// What the previous decision ended up knowing, one entry per candidate it scored: the tokens
    /// it ran, the state it left the model in, and the log-probability accumulated over them.
    ///
    /// A keystroke rescores candidates that are mostly the previous keystroke's candidates with a
    /// character added, so most of each candidate has already been run. Measured on the sentence
    /// eval driven through the real runtime, 61% of the characters scored are a prefix of
    /// something the previous keystroke scored. Keeping only the previous decision bounds this to
    /// one entry per candidate; keeping more would grow without a ceiling for reuse that was not
    /// measured to be there.
    pub(crate) resume: Vec<(Vec<u32>, Arc<TailRun>)>,
    /// A spare vector whose allocation is reused while the previous decision remains available
    /// for prefix matching.
    pub(crate) resume_scratch: Vec<(Vec<u32>, Arc<TailRun>)>,
    /// `best_where` only needs scores until it has chosen an index; keep that short result buffer
    /// between keystrokes instead of allocating it for every decision.
    pub(crate) scores_scratch: Vec<f32>,
}

impl Reranker {
    pub fn new(model: Arc<SentenceModel>) -> Self {
        Self {
            model,
            cached: None,
            resume: Vec::new(),
            resume_scratch: Vec::new(),
            scores_scratch: Vec::new(),
        }
    }

    pub fn model(&self) -> &SentenceModel {
        &self.model
    }

    /// The index of the candidate the model prefers over the engine's leader, or `None` to leave the
    /// order alone. Both slices are the engine's candidate list in its own order, and the returned
    /// index refers to that list.
    ///
    /// **This infers comparability from character count, which is only a proxy, and it is the wrong
    /// proxy as soon as the engine corrects what was typed.** Candidates of the leader's length are
    /// assumed to have answered the same key. That holds while one key has one segmentation, and
    /// breaks when it does not: `xian` reads as 现 or as 西安, both consuming the whole key, and this
    /// method compares only whichever of the two matches the leader. Fuzzy pinyin also makes the
    /// dictionary test wrong here, because `sources` cannot say whether a dictionary hit landed on
    /// the key the user typed or on a fuzz-expanded variant of it.
    ///
    /// Kept because it is the one call an engine using the same source numbering can make without
    /// supplying anything else. An engine that corrects input should use [`Reranker::best_where`],
    /// which asks for both facts instead of guessing at them.
    pub fn best(&mut self, context: &str, texts: &[&str], sources: &[u8]) -> Option<usize> {
        let width = texts.first().map_or(0, |text| text.chars().count());
        self.best_where(context, texts, |index| CandidateFacts {
            answers_key: texts
                .get(index)
                .is_some_and(|text| text.chars().count() == width),
            trusted_dictionary_hit: sources
                .get(index)
                .is_some_and(|source| DICTIONARY_SOURCES.contains(source)),
        })
    }

    /// The same decision with the caller supplying what only the engine knows about each candidate.
    ///
    /// See [`CandidateFacts`] for what the two fields mean and why neither can be derived from the
    /// candidate strings. The comparable set is every candidate that answers the key, capped at nine;
    /// the decision is declined when the leader is an exact dictionary hit on the typed key, when the
    /// leader does not itself answer the key, or when fewer than two candidates do.
    ///
    /// **Candidates that answer the key are compared even when they are different numbers of
    /// characters.** The scores are per-character means for exactly this reason. A summed
    /// log-probability would rank the shortest candidate first regardless of quality, which is what
    /// makes a mixed-length list dangerous — but "mixed length" was never the thing to exclude.
    /// Prefixes and predictive completions have to be excluded because they answer a different key,
    /// not because of their length, and `answers_key` says so directly.
    pub fn best_where(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: impl Fn(usize) -> CandidateFacts,
    ) -> Option<usize> {
        self.best_where_margin(context, texts, facts, 0.0)
    }

    /// [`Reranker::best_where`] with a minimum lead. A promotion is declined unless the model's
    /// preferred candidate beats the engine's leader by at least `min_margin` in mean
    /// log-probability per character. A zero margin reproduces `best_where`.
    ///
    /// Small score gaps are noise: the shared benchmark showed the 4M model lowering top-1 when it
    /// was allowed to act on them, while clear leads still help. The caller picks the margin because
    /// it knows which model and latency budget the decision belongs to.
    pub fn best_where_margin(
        &mut self,
        context: &str,
        texts: &[&str],
        facts: impl Fn(usize) -> CandidateFacts,
        min_margin: f32,
    ) -> Option<usize> {
        let considered = comparable(texts.len(), &facts)?;
        let mut scores = std::mem::take(&mut self.scores_scratch);
        self.score_indices_into(
            context,
            texts,
            considered.as_slice().iter().copied(),
            &mut scores,
        );
        let mut best = 0;
        for (index, score) in scores.iter().enumerate() {
            if *score > scores[best] {
                best = index;
            }
        }
        let promoted = (best != 0 && scores[best] - scores[0] >= min_margin)
            .then_some(considered.as_slice()[best]);
        self.scores_scratch = scores;
        promoted
    }

    /// Mean log-probability per character for each candidate, in the order given.
    ///
    /// Exposed because the model's opinion is only one term in the decision: combining it with the
    /// evidence the engine already has is a different question from letting it decide alone, and
    /// answering that question needs the numbers rather than the verdict.
    pub fn log_probabilities(&mut self, context: &str, candidates: &[&str]) -> Vec<f32> {
        self.score(context, candidates)
    }

    fn score(&mut self, context: &str, candidates: &[&str]) -> Vec<f32> {
        let mut scores = Vec::new();
        self.score_indices_into(context, candidates, 0..candidates.len(), &mut scores);
        scores
    }

    fn score_indices_into(
        &mut self,
        context: &str,
        candidates: &[&str],
        indices: impl Iterator<Item = usize> + Clone,
        scores: &mut Vec<f32>,
    ) {
        let longest = indices
            .clone()
            .map(|index| candidates[index].chars().count())
            .max()
            .unwrap_or(0);
        // Keyed on the tokens rather than the context string, because two different contexts that
        // truncate to the same window are the same prefix and must not invalidate each other.
        let tokens = self.model.prefix_tokens(context, longest);
        let matched = self
            .cached
            .as_ref()
            .is_some_and(|(cached, _)| *cached == tokens);
        if !matched {
            let prefix = self.model.run_prefix(&tokens);
            self.cached = Some((tokens, prefix));
        }
        let (_, prefix) = self.cached.as_ref().expect("prefix was just computed");
        let capacity = indices.size_hint().0;
        let mut previous =
            std::mem::replace(&mut self.resume, std::mem::take(&mut self.resume_scratch));
        self.resume.clear();
        // Every resume point hangs off the old prefix and means nothing against a new one.
        if !matched {
            previous.clear();
        }
        scores.clear();
        scores.reserve_exact(capacity);
        let limit = self
            .model
            .config
            .context
            .saturating_sub(prefix.length)
            .max(1);
        for index in indices {
            let tail = self.model.encode_limited(candidates[index], limit);
            if tail.is_empty() {
                scores.push(f32::NEG_INFINITY);
                continue;
            }
            // 完全命中优先保留旧轮状态，再复用本轮已有结果；两组状态都供后续候选借用。
            if let Some((_, run)) = previous
                .iter()
                .chain(&self.resume)
                .find(|(tokens, _)| tokens == &tail)
            {
                let total = run.cumulative[tail.len() - 1];
                scores.push(total / tail.len() as f32);
                self.resume.push((tail, Arc::clone(run)));
                continue;
            }

            // 选两轮的最长共享前缀；max_by_key 取最后一个同长项，旧轮顺序和优先级保持。
            let shared = self
                .resume
                .iter()
                .chain(&previous)
                .map(|(tokens, run)| {
                    let agreed = tokens
                        .iter()
                        .zip(&tail)
                        .take_while(|(left, right)| left == right)
                        .count();
                    (run, agreed.min(tail.len() - 1))
                })
                .max_by_key(|(_, agreed)| *agreed);

            let run = match shared {
                Some((previous, agreed)) if agreed > 0 => {
                    let (from, carried) = previous.resume(agreed, &self.model);
                    let rest = self.model.run_tail(&from, &tail[agreed..]);
                    previous.joined(agreed, carried, rest)
                }
                _ => self.model.run_tail(prefix, &tail),
            };
            let total = run.cumulative[run.cumulative.len() - 1];
            scores.push(total / tail.len() as f32);
            self.resume.push((tail, Arc::new(run)));
        }
        previous.clear();
        self.resume_scratch = previous;
    }
}

/// Whether the model should be consulted at all, given the sources the engine reported in order.
///
/// Only the leading candidate matters. When it is an exact dictionary hit on the whole key it
/// carries corpus frequency the model does not have, and reranking measurably loses accuracy.
///
/// **Sources alone cannot answer this for an engine that corrects input.** A hit on a fuzz-expanded
/// key reports the same source as a hit on the typed key while carrying frequency for a word the
/// user did not ask for. Such an engine should decide with [`CandidateFacts::trusted_dictionary_hit`]
/// instead, which asks the question this function can only approximate.
pub fn should_rerank(sources: &[u8]) -> bool {
    match sources.first() {
        Some(source) => !DICTIONARY_SOURCES.contains(source),
        None => false,
    }
}

//! 重排器:用语言模型给引擎候选重新排序。
//!
//! 可比集的判定与提位阈值在这里;每轮的续跑状态与批式评分计划也归它持有,
//! 前向算术交给 [`crate::forward`]。

use std::sync::Arc;

use crate::forward::{BatchContext, BatchRow, HiddenState, Prefix, TailRun, TerminalState};
use crate::model::SentenceModel;
use crate::{CandidateFacts, DICTIONARY_SOURCES};
/// How many comparable candidates are scored. The engine rarely returns more than a handful that
/// answer the same key — two in almost every real case — so this is a ceiling on the worst case
/// rather than a limit that normally binds.
pub(crate) const COMPARED: usize = 9;

/// The bounded set of candidate indices that answer the current key. Keeping the ceiling in the
/// type avoids allocating a temporary `Vec` before every reranking call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Compared {
    pub(crate) indices: [usize; COMPARED],
    pub(crate) len: usize,
}

impl Compared {
    fn new() -> Self {
        Self {
            indices: [0; COMPARED],
            len: 0,
        }
    }

    fn push(&mut self, index: usize) {
        self.indices[self.len] = index;
        self.len += 1;
    }

    pub(crate) fn as_slice(&self) -> &[usize] {
        &self.indices[..self.len]
    }
}

impl std::fmt::Debug for Reranker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reranker")
            .field("model", &self.model)
            .field("prefix_cached", &self.cached.is_some())
            .finish()
    }
}

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
    /// Small score gaps are noise: the shared benchmark showed the 8M model lowering top-1 when it
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
        // Plan first, with tokens alone: which earlier run each candidate borrows from and how far,
        // by the same rules and the same tie-breaking as when every candidate was run on its own
        // in turn. Then run every new position of the decision in one batch, and only then put the
        // runs together. A position's state depends only on the tokens before it, so the states are
        // the ones the one-at-a-time loop computed; the cumulative sums are rebuilt in the order it
        // added them, so the scores are the same f32.
        let mut planned: Vec<(Vec<u32>, Known)> = Vec::with_capacity(capacity);
        let mut candidates_planned: Vec<bool> = Vec::with_capacity(capacity);
        let mut runs: Vec<PlannedRun> = Vec::new();
        for index in indices {
            let tail = self.model.encode_limited(candidates[index], limit);
            if tail.is_empty() {
                candidates_planned.push(false);
                continue;
            }
            // An exact match keeps the previous decision's run first, then this decision's.
            let exact = previous
                .iter()
                .enumerate()
                .map(|(index, (tokens, _))| (tokens, Known::Previous(index)))
                .chain(planned.iter().map(|(tokens, known)| (tokens, *known)))
                .find(|(tokens, _)| **tokens == tail)
                .map(|(_, known)| known);
            let known = exact.unwrap_or_else(|| {
                // The longest shared prefix across both decisions; `max_by_key` keeps the last of
                // equal lengths, so the previous decision's runs win ties, as they always have.
                let shared = planned
                    .iter()
                    .map(|(tokens, known)| (tokens, *known))
                    .chain(
                        previous
                            .iter()
                            .enumerate()
                            .map(|(index, (tokens, _))| (tokens, Known::Previous(index))),
                    )
                    .map(|(tokens, known)| {
                        let agreed = tokens
                            .iter()
                            .zip(&tail)
                            .take_while(|(left, right)| left == right)
                            .count();
                        (known, agreed.min(tail.len() - 1))
                    })
                    .max_by_key(|(_, agreed)| *agreed);
                runs.push(PlannedRun {
                    entry: planned.len(),
                    from: shared.filter(|(_, agreed)| *agreed > 0),
                    anchor: (0, None, 0),
                    rows: Vec::new(),
                    terminal: None,
                });
                Known::Computed(runs.len() - 1)
            });
            candidates_planned.push(true);
            planned.push((tail, known));
        }

        let model = &*self.model;
        let mut batch = Batch {
            contexts: vec![BatchContext::of(prefix)],
            rows: Vec::new(),
            runs,
            previous_terminals: vec![None; previous.len()],
        };
        for run in 0..batch.runs.len() {
            let (tail, _) = &planned[batch.runs[run].entry];
            let (anchor, agreed) = match batch.runs[run].from {
                Some((known, agreed)) => (batch.anchor(&previous, &planned, known, agreed), agreed),
                None => ((0, None, 0), 0),
            };
            batch.runs[run].anchor = anchor;
            let (context, mut parent, mut depth) = anchor;
            for &token in &tail[agreed..tail.len() - 1] {
                batch.rows.push(BatchRow {
                    token,
                    parent,
                    depth,
                    context,
                });
                parent = Some(batch.rows.len() - 1);
                depth += 1;
                batch.runs[run].rows.push(batch.rows.len() - 1);
            }
        }
        let states = model.run_batch(&batch.contexts, &batch.rows);
        let n_embd = model.config.n_embd;
        let hidden: Vec<Arc<HiddenState>> = states
            .hidden
            .chunks_exact(n_embd)
            .map(|row| Arc::new(HiddenState::new(row.to_vec())))
            .collect();
        let terminal_of = |row: usize| TerminalState {
            keys: states
                .keys
                .iter()
                .map(|layer| layer[row * n_embd..(row + 1) * n_embd].to_vec())
                .collect(),
            values: states
                .values
                .iter()
                .map(|layer| layer[row * n_embd..(row + 1) * n_embd].to_vec())
                .collect(),
            hidden: Arc::clone(&hidden[row]),
        };

        // Every state a log-probability is about to be read from, normalized together.
        let mut pending: Vec<&Arc<HiddenState>> = Vec::new();
        let starts: Vec<Arc<HiddenState>> = batch
            .runs
            .iter()
            .map(|run| match run.from {
                Some((known, agreed)) => {
                    batch.state(&previous, &planned, &hidden, prefix, known, agreed)
                }
                None => Arc::clone(&prefix.last_hidden),
            })
            .collect();
        for state in hidden.iter().chain(&starts) {
            if state.log_normalizer.get().is_none()
                && !pending.iter().any(|seen| Arc::ptr_eq(seen, state))
            {
                pending.push(state);
            }
        }
        if !pending.is_empty() {
            let mut gathered = Vec::with_capacity(pending.len() * n_embd);
            for state in &pending {
                gathered.extend_from_slice(&state.values);
            }
            let mut normalizers = Vec::with_capacity(pending.len());
            model.log_normalizers_into(&gathered, &mut normalizers);
            for (state, normalizer) in pending.iter().zip(normalizers) {
                // Unset a moment ago, and nothing else holds this decision's states.
                let _ = state.log_normalizer.set(normalizer);
            }
        }

        for (index, terminal) in batch.previous_terminals.iter().enumerate() {
            if let Some(row) = *terminal {
                let _ = previous[index].1.terminal.set(terminal_of(row));
            }
        }
        let mut built: Vec<Arc<TailRun>> = Vec::with_capacity(batch.runs.len());
        for (run, start) in batch.runs.iter().zip(&starts) {
            let (tail, _) = &planned[run.entry];
            let (context, parent, _) = run.anchor;
            let context = batch.contexts[context];
            let chain = match (run.rows.last(), parent) {
                (Some(&last), _) | (None, Some(last)) => states.chains[last].as_slice(),
                (None, None) => &[],
            };
            let width = (context.length() + chain.len()) * n_embd;
            let copy = |stored: &[Vec<f32>], computed: &[Vec<f32>], last: Option<&[Vec<f32>]>| {
                stored
                    .iter()
                    .zip(computed)
                    .enumerate()
                    .map(|(layer, (stored, computed))| {
                        let mut out = Vec::with_capacity(width);
                        out.extend_from_slice(&stored[..context.stored * n_embd]);
                        if let Some(last) = last {
                            out.extend_from_slice(&last[layer]);
                        }
                        for &row in chain {
                            out.extend_from_slice(&computed[row * n_embd..(row + 1) * n_embd]);
                        }
                        out
                    })
                    .collect::<Vec<_>>()
            };
            let keys = copy(
                context.keys,
                &states.keys,
                context.terminal.map(|last| last.keys.as_slice()),
            );
            let values = copy(
                context.values,
                &states.values,
                context.terminal.map(|last| last.values.as_slice()),
            );
            let own = run.rows.iter().map(|&row| &hidden[row]);
            let (agreed, source) = match run.from {
                Some((known, agreed)) => (
                    agreed,
                    Some(match known {
                        Known::Previous(index) => Arc::clone(&previous[index].1),
                        Known::Computed(earlier) => Arc::clone(&built[earlier]),
                    }),
                ),
                None => (0, None),
            };
            // The same sums `run_tail` and `TailRun::joined` produced: a fresh running total over
            // this run's own tokens, each added to the total it continues.
            let mut running = model.log_probability(start, tail[agreed]);
            let mut rest = Vec::with_capacity(tail.len() - agreed);
            rest.push(running);
            for (state, &next) in own.clone().zip(&tail[agreed + 1..]) {
                running += model.log_probability(state, next);
                rest.push(running);
            }
            let tail_run = match source {
                None => TailRun {
                    base: prefix.length,
                    keys,
                    values,
                    hidden: own.cloned().collect(),
                    cumulative: rest,
                    pending: tail[tail.len() - 1],
                    terminal: OnceLock::new(),
                },
                Some(source) => {
                    let carried = source.cumulative[agreed - 1];
                    let terminal = (agreed > source.hidden.len()).then(|| {
                        Arc::clone(
                            &source
                                .terminal
                                .get()
                                .expect("a run continued past its last state has its terminal")
                                .hidden,
                        )
                    });
                    TailRun {
                        base: source.base,
                        keys,
                        values,
                        hidden: source
                            .hidden
                            .iter()
                            .take(agreed)
                            .cloned()
                            .chain(terminal)
                            .chain(own.cloned())
                            .collect(),
                        cumulative: source.cumulative[..agreed]
                            .iter()
                            .copied()
                            .chain(rest.iter().map(|value| carried + value))
                            .collect(),
                        pending: tail[tail.len() - 1],
                        terminal: OnceLock::new(),
                    }
                }
            };
            if let Some(row) = run.terminal {
                let _ = tail_run.terminal.set(terminal_of(row));
            }
            built.push(Arc::new(tail_run));
        }

        let mut entries = planned.into_iter();
        for scored in candidates_planned {
            if !scored {
                scores.push(f32::NEG_INFINITY);
                continue;
            }
            let (tail, known) = entries.next().expect("one entry per scored candidate");
            let run = match known {
                Known::Previous(index) => Arc::clone(&previous[index].1),
                Known::Computed(index) => Arc::clone(&built[index]),
            };
            let total = run.cumulative[tail.len() - 1];
            scores.push(total / tail.len() as f32);
            self.resume.push((tail, run));
        }
        previous.clear();
        self.resume_scratch = previous;
    }
}

/// Which candidates may be compared, or `None` when the decision should be declined.
///
/// Separate from scoring and free of the model, because this is the part that decides what the
/// measurements mean: every published accuracy number for this crate is a consequence of which
/// candidates were allowed into the comparison, so it is worth being able to test on its own.
pub(crate) fn comparable(
    count: usize,
    facts: &impl Fn(usize) -> CandidateFacts,
) -> Option<Compared> {
    if count == 0 {
        return None;
    }
    let leader = facts(0);
    // An exact hit on the typed key carries frequency evidence the model does not have. A leader
    // that does not answer the key leaves nothing to promote it over: the comparison would be
    // between readings of different keys.
    if leader.trusted_dictionary_hit || !leader.answers_key {
        return None;
    }
    let mut considered = Compared::new();
    for index in (0..count).filter(|&index| facts(index).answers_key) {
        if considered.len == COMPARED {
            break;
        }
        considered.push(index);
    }
    (considered.len >= 2).then_some(considered)
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

/// A run a candidate can be scored from: one the previous decision left, or one this decision
/// computes.
#[derive(Clone, Copy)]
enum Known {
    Previous(usize),
    Computed(usize),
}

/// A candidate this decision runs: its entry among the decision's candidates, the run it continues
/// and after how many tokens (`None` to start from the prefix), and where its rows went.
struct PlannedRun {
    entry: usize,
    from: Option<(Known, usize)>,
    /// The context, the batch row and the depth the run's first own row continues.
    anchor: (usize, Option<usize>, usize),
    rows: Vec<usize>,
    /// The batch row that runs this candidate's last token, when a later candidate continues past it.
    terminal: Option<usize>,
}

/// A decision's positions being laid out as one batch.
struct Batch<'a> {
    contexts: Vec<BatchContext<'a>>,
    rows: Vec<BatchRow>,
    runs: Vec<PlannedRun>,
    /// The batch row that runs a previous run's last token, for previous runs continued past it.
    previous_terminals: Vec<Option<usize>>,
}

impl<'a> Batch<'a> {
    /// Where a row continuing `known` after `agreed` tokens hangs: its context, its parent row and
    /// its depth. Adds the context, or the row that runs a terminal token, when it is not there yet.
    fn anchor(
        &mut self,
        previous: &'a [(Vec<u32>, Arc<TailRun>)],
        planned: &[(Vec<u32>, Known)],
        known: Known,
        agreed: usize,
    ) -> (usize, Option<usize>, usize) {
        match known {
            Known::Previous(index) => {
                let run = &*previous[index].1;
                let context = |stored, terminal| BatchContext {
                    keys: &run.keys,
                    values: &run.values,
                    stored,
                    terminal,
                };
                if agreed <= run.hidden.len() {
                    self.contexts.push(context(run.base + agreed, None));
                    return (self.contexts.len() - 1, None, 0);
                }
                if let Some(terminal) = run.terminal.get() {
                    self.contexts
                        .push(context(run.base + run.hidden.len(), Some(terminal)));
                    return (self.contexts.len() - 1, None, 0);
                }
                let row = match self.previous_terminals[index] {
                    Some(row) => row,
                    None => {
                        self.contexts
                            .push(context(run.base + run.hidden.len(), None));
                        self.rows.push(BatchRow {
                            token: run.pending,
                            parent: None,
                            depth: 0,
                            context: self.contexts.len() - 1,
                        });
                        self.previous_terminals[index] = Some(self.rows.len() - 1);
                        self.rows.len() - 1
                    }
                };
                (self.rows[row].context, Some(row), 1)
            }
            Known::Computed(index) => {
                let length = planned[self.runs[index].entry].0.len();
                let start = self.runs[index].from.map_or(0, |(_, agreed)| agreed);
                if agreed <= start {
                    return match self.runs[index].from {
                        Some((known, _)) => self.anchor(previous, planned, known, agreed),
                        None => (0, None, 0),
                    };
                }
                if agreed < length {
                    let row = self.runs[index].rows[agreed - start - 1];
                    return (self.rows[row].context, Some(row), self.rows[row].depth + 1);
                }
                let row = match self.runs[index].terminal {
                    Some(row) => row,
                    None => {
                        let (context, parent, depth) =
                            self.anchor(previous, planned, known, length - 1);
                        self.rows.push(BatchRow {
                            token: planned[self.runs[index].entry].0[length - 1],
                            parent,
                            depth,
                            context,
                        });
                        self.runs[index].terminal = Some(self.rows.len() - 1);
                        self.rows.len() - 1
                    }
                };
                (self.rows[row].context, Some(row), self.rows[row].depth + 1)
            }
        }
    }

    /// The state that predicts token `agreed` of a candidate continuing `known`.
    fn state(
        &self,
        previous: &[(Vec<u32>, Arc<TailRun>)],
        planned: &[(Vec<u32>, Known)],
        hidden: &[Arc<HiddenState>],
        prefix: &Prefix<'_>,
        known: Known,
        agreed: usize,
    ) -> Arc<HiddenState> {
        match known {
            Known::Previous(index) => {
                let run = &previous[index].1;
                if agreed <= run.hidden.len() {
                    return Arc::clone(&run.hidden[agreed - 1]);
                }
                match run.terminal.get() {
                    Some(terminal) => Arc::clone(&terminal.hidden),
                    None => Arc::clone(
                        &hidden[self.previous_terminals[index].expect("terminal row was planned")],
                    ),
                }
            }
            Known::Computed(index) => {
                let run = &self.runs[index];
                let length = planned[run.entry].0.len();
                let start = run.from.map_or(0, |(_, agreed)| agreed);
                if agreed <= start {
                    return match run.from {
                        Some((known, _)) => {
                            self.state(previous, planned, hidden, prefix, known, agreed)
                        }
                        None => Arc::clone(&prefix.last_hidden),
                    };
                }
                if agreed < length {
                    return Arc::clone(&hidden[run.rows[agreed - start - 1]]);
                }
                Arc::clone(&hidden[run.terminal.expect("terminal row was planned")])
            }
        }
    }
}

// 固定 #53 的完整评分流程，只重命名并移除说明；对照时保留同样的跨轮状态。
use crate::{Reranker, SentenceModel};
use std::sync::Arc;

impl Reranker {
    fn legacy_score(&mut self, context: &str, candidates: &[&str]) -> Vec<f32> {
        let longest = candidates
            .iter()
            .map(|text| text.chars().count())
            .max()
            .unwrap_or(0);
        let tokens = self.model.prefix_tokens(context, longest);
        let matched = self
            .cached
            .as_ref()
            .is_some_and(|(cached, _)| *cached == tokens);
        if !matched {
            let prefix = self.model.run_prefix(&tokens);
            self.cached = Some((tokens, prefix));
            self.resume.clear();
        }
        let (_, prefix) = self.cached.as_ref().expect("prefix was just computed");
        let mut next = Vec::with_capacity(candidates.len());
        let mut scores = Vec::with_capacity(candidates.len());
        for text in candidates {
            let mut tail = self.model.encode(text);
            if tail.is_empty() {
                scores.push(f32::NEG_INFINITY);
                continue;
            }
            let limit = self
                .model
                .config
                .context
                .saturating_sub(prefix.length)
                .max(1);
            tail.truncate(limit);

            if let Some((_, run)) = self
                .resume
                .iter()
                .chain(&next)
                .find(|(tokens, _)| tokens == &tail)
            {
                let total = run.cumulative[tail.len() - 1];
                scores.push(total / tail.len() as f32);
                next.push((tail, Arc::clone(run)));
                continue;
            }

            let shared = next
                .iter()
                .chain(&self.resume)
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
            next.push((tail, Arc::new(run)));
        }
        self.resume = next;
        scores
    }
}

#[test]
fn bounded_encoding_preserves_each_rounds_score_bits_and_cache_order() {
    let model = super::cache_test_model();
    let mut old = Reranker::new(model.clone());
    let mut new = Reranker::new(model.clone());
    for context in ["", "我", "很", "你好我很好我", "🙂你好我很好我"] {
        for length in [0, 1, 2, 6, 7, 8, 9, 16, 256] {
            let text = "你🙂好A吗".chars().cycle().take(length).collect::<String>();
            let extended = format!("{text}很");
            for candidates in [
                vec![text.as_str(), "你好", "你吗", ""],
                vec!["你吗", text.as_str(), text.as_str(), extended.as_str()],
                vec![extended.as_str(), text.as_str(), "🙂", ""],
                vec![],
            ] {
                let expected = old.legacy_score(context, &candidates);
                let actual = new.log_probabilities(context, &candidates);
                assert_eq!(
                    actual
                        .iter()
                        .map(|score| score.to_bits())
                        .collect::<Vec<_>>(),
                    expected
                        .iter()
                        .map(|score| score.to_bits())
                        .collect::<Vec<_>>()
                );
                assert_eq!(new.resume.len(), old.resume.len());
                for ((tokens, run), (prior, old_run)) in new.resume.iter().zip(&old.resume) {
                    assert_eq!(tokens, prior);
                    assert_eq!(run.base, old_run.base);
                    assert_eq!(run.pending, old_run.pending);
                    assert_eq!(run.keys, old_run.keys);
                    assert_eq!(run.values, old_run.values);
                    assert_eq!(run.cumulative, old_run.cumulative);
                }
            }
        }
    }
}

#[test]
fn limited_encoding_is_the_original_unicode_token_prefix() {
    let model: std::sync::Arc<SentenceModel> = super::cache_test_model();
    for text in [
        "",
        "你好",
        "A你🙂好𠀀",
        "e\u{301}你",
        "你好吗我很好你好吗我很好",
    ] {
        let expected = model.encode(text);
        for limit in [0, 1, 2, 3, 7, 8, 15, 32, usize::MAX] {
            assert_eq!(
                model.encode_limited(text, limit),
                expected[..expected.len().min(limit)]
            );
        }
    }
}

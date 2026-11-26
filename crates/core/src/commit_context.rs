//! 用词典里跨上屏边界的整词,校验下一段读法。

use std::collections::HashMap;

use lueying_dict::{Dictionary, syllables};

use crate::Candidate;

pub(crate) struct CommitAnchor {
    word: String,
    display: Option<String>,
    key: String,
}

impl CommitAnchor {
    pub(crate) fn new(cand: &Candidate, word: &str) -> Option<Self> {
        let key = cand.learn_key.as_deref()?;
        // 单字容易偶然拼出许多词,不作为封住首位的锚点。
        if word.chars().count() < 2 || !key.split('\'').all(syllables::is_syllable) {
            return None;
        }
        Some(Self {
            word: word.to_string(),
            display: (cand.text != word).then(|| cand.text.clone()),
            key: key.to_string(),
        })
    }

    pub(crate) fn word(&self) -> &str {
        &self.word
    }

    pub(crate) fn matches_tail(&self, text: &str) -> bool {
        text.ends_with(&self.word)
            || self
                .display
                .as_deref()
                .is_some_and(|word| text.ends_with(word))
    }

    pub(crate) fn promote_attested(&self, dict: &Dictionary, cands: &mut [Candidate]) {
        let Some(leader) = cands.first() else {
            return;
        };
        if leader.learned_enough() {
            return;
        }
        let mut prefix = self.key.clone();
        prefix.push('\'');
        if !dict.has_key_prefix(&prefix) {
            return;
        }
        // 同音候选共用一次查词;必须同时对上提交的文字与两段拼音。
        let mut by_reading = HashMap::new();
        let mut found: Option<usize> = None;
        for (index, cand) in cands.iter().enumerate() {
            if !cand.full
                || !cand.exact
                || cand.rejected
                || cand.completion
                || cand.no_learn
                || cand.quick
            {
                continue;
            }
            let Some(key) = cand.learn_key.as_deref() else {
                continue;
            };
            let entries = by_reading.entry(key).or_insert_with(|| {
                let mut joined = prefix.clone();
                joined.push_str(key);
                dict.lookup_exact(&joined)
            });
            if entries
                .iter()
                .any(|e| e.freq > 0 && e.word.strip_prefix(&self.word) == Some(cand.text.as_str()))
            {
                if let Some(other) = found {
                    if cands[other].text != cand.text {
                        // 两个候选都能接成整词,交给原排序决定。
                        return;
                    }
                } else {
                    found = Some(index);
                }
            }
        }
        drop(by_reading);
        if let Some(index) = found {
            cands[..=index].rotate_right(1);
            cands[0].attested_context = true;
        }
    }
}

//! 打字习惯:模糊组的适应与快捷词抑制。
//!
//! 这些不是词的记忆,与 `user_learn` 的词记忆互不影响。计数按周减半,不做遗忘曲线;
//! 行为沿用旧设计,等打字习惯的通道单独重做时再替换。

use std::collections::{BTreeSet, HashMap};

use lueying_dict::syllables::FUZZY_GROUPS;

const WEEK_SECS: u64 = 7 * 24 * 3600;
/// 每组每选一次给模糊候选加的分,以及单组/总计的上限。
const GROUP_BONUS_STEP: f64 = 0.12;
const GROUP_BONUS_COUNT_CAP: u32 = 5;
const GROUP_BONUS_MAX: f64 = 0.6;
/// 前两次由词记忆负责;同一组连续选到第三次才开始整组上浮。
const GROUP_BONUS_FROM: u32 = 3;
pub(crate) const GROUP_COUNT: usize = FUZZY_GROUPS.len();

/// 按周减半的计数。
#[derive(Clone, Copy, Default)]
pub(crate) struct Weekly {
    pub count: u32,
    pub ts: u64,
}

impl Weekly {
    fn effective(self, now: u64) -> u32 {
        let weeks = now.saturating_sub(self.ts) / WEEK_SECS;
        self.count.checked_shr(weeks.min(31) as u32).unwrap_or(0)
    }

    fn bump(&mut self, now: u64) {
        self.count = self.effective(now).saturating_add(1);
        self.ts = now;
    }
}

/// 一个模糊组的打字习惯:连续选择次数加见过这次选择的左邻。
/// `contexts` 为 `None` 表示旧档案,不限作用域;集合里的 `None` 表示句首。
#[derive(Default)]
pub(crate) struct GroupHabit {
    pub stat: Weekly,
    pub contexts: Option<BTreeSet<Option<[char; 2]>>>,
}

pub(crate) struct Typing {
    /// 各模糊组的连续选择次数。
    pub groups: [GroupHabit; GROUP_COUNT],
    /// (键, 快捷动作 id) → 选择字面原文的次数。
    pub literal: HashMap<(String, String), Weekly>,
}

impl Default for Typing {
    fn default() -> Self {
        Self {
            groups: std::array::from_fn(|_| GroupHabit::default()),
            literal: HashMap::new(),
        }
    }
}

impl Typing {
    pub(crate) fn learn_groups(
        &mut self,
        mask: u8,
        scope: Option<Option<[char; 2]>>,
        now: u64,
    ) {
        for (index, habit) in self.groups.iter_mut().enumerate() {
            if mask & (1 << index) != 0 {
                habit.stat.bump(now);
                // 左邻无从判断时计数仍然累计(用户确实选了模糊读法),
                // 但不记进作用域——防止模糊语境扩大回放范围。
                let Some(scope) = scope else { continue };
                habit
                    .contexts
                    .get_or_insert_with(BTreeSet::new)
                    .insert(scope);
            }
        }
    }

    pub(crate) fn group_bonus(
        &self,
        mask: u8,
        scope: Option<Option<[char; 2]>>,
        now: u64,
    ) -> f64 {
        if mask == 0 {
            return 0.0;
        }
        let Some(scope) = scope else { return 0.0 };
        let mut bonus = 0.0;
        for (index, habit) in self.groups.iter().enumerate() {
            if mask & (1 << index) == 0 {
                continue;
            }
            // 组级习惯只在见过的左邻下生效;没见过的语境按没学过的习惯对待。
            if habit
                .contexts
                .as_ref()
                .is_some_and(|contexts| !contexts.contains(&scope))
            {
                continue;
            }
            let effective = habit.stat.effective(now);
            if effective >= GROUP_BONUS_FROM {
                bonus += f64::from(effective.min(GROUP_BONUS_COUNT_CAP)) * GROUP_BONUS_STEP;
            }
        }
        bonus.min(GROUP_BONUS_MAX)
    }

    pub(crate) fn group_stats(&self, now: u64) -> Vec<(&'static str, u32)> {
        FUZZY_GROUPS
            .iter()
            .copied()
            .zip(self.groups.iter().map(|habit| habit.stat.effective(now)))
            .collect()
    }

    pub(crate) fn learn_literal(&mut self, key: &str, action: &str, now: u64) {
        self.literal
            .entry((key.to_string(), action.to_string()))
            .or_default()
            .bump(now);
    }

    pub(crate) fn literal_picks(&self, key: &str, action: &str, now: u64) -> u32 {
        self.literal
            .get(&(key.to_string(), action.to_string()))
            .map_or(0, |stat| stat.effective(now))
    }
}

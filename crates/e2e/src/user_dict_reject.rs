//! 用户词库 E2E:跳过负分与大词库查询。
//!
//! 负分的写入、衰减、重新选中与 Shift+Delete 清理都走真实学习路径。

use std::sync::RwLock;
use std::time::Instant;

use lueying_engine::{Key, UserDict};

use super::*;

/// E2E:跳过负分的写入/失效/清除,以及大词库查询。
pub(super) fn rejection_and_scale(shared: &Arc<Dictionary>, checks: &mut Checks, now: u64) {
    let dict = Arc::clone(shared);

    // 同键跳过:两条同键用户词,甲甲 频次略高。对照引擎首选甲甲;给甲甲
    // 导入足够的跳过次数后,首选必须让给乙乙。甲甲学过的词频不低,
    // 首位的精确/学习保护也必须为负分让位。
    let pair = "jia'jia\t甲甲\t100000\t0\njia'jia\t乙乙\t99000\t0\n";
    let mut control = Engine::new(Arc::clone(&dict), Config::default());
    control.import_user(pair);
    for ch in "jiajia".chars() {
        control.process(Key::Letter(ch));
    }
    let control_top = control.candidates().first().cloned().unwrap_or_default();
    let mut disliked = Engine::new(Arc::clone(&dict), Config::default());
    disliked.import_user(&format!("{pair}@reject\tjia'jia\t甲甲\t100\t{now}\n"));
    for ch in "jiajia".chars() {
        disliked.process(Key::Letter(ch));
    }
    let disliked_top = disliked.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "跳过候选改变首选",
        control_top == "甲甲" && disliked_top == "乙乙",
        format!("无跳过 {control_top} → 有跳过 {disliked_top}"),
    );

    // 过期负分自然失效:四十周前的跳过记录按周衰减后归零,不能永久压住候选。
    let stale_ts = now.saturating_sub(40 * 7 * 24 * 3600);
    let mut stale = Engine::new(Arc::clone(&dict), Config::default());
    stale.import_user(&format!("{pair}@reject\tjia'jia\t甲甲\t100\t{stale_ts}\n"));
    for ch in "jiajia".chars() {
        stale.process(Key::Letter(ch));
    }
    let stale_top = stale.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "过期跳过记录失效",
        stale_top == "甲甲",
        format!("四十周后首选 {stale_top}"),
    );

    // 选择后面的候选时,被越过的同键候选写入 @reject;该记录随词库往返。
    let mut skipped = Engine::new(Arc::clone(&dict), Config::default());
    skipped.import_user(pair);
    for ch in "jiajia".chars() {
        skipped.process(Key::Letter(ch));
    }
    let recorded = pick_word(&mut skipped, "乙乙");
    let skipped_export = skipped.export_user();
    let reject_line = skipped_export
        .lines()
        .find(|l| l.starts_with("@reject\tjia'jia\t甲甲\t"))
        .unwrap_or_default()
        .to_string();
    let mut restored = Engine::new(Arc::clone(&dict), Config::default());
    restored.import_user(&skipped_export);
    let restored_export = restored.export_user();
    push(
        checks,
        "跳过记录导出导入往返",
        recorded
            && reject_line.starts_with("@reject\tjia'jia\t甲甲\t1\t")
            && restored_export.contains(&reject_line),
        reject_line,
    );

    // 重新选中被跳过的词:旧负分必须清掉,否则下次同键仍被压在后面。
    let mut forgive = Engine::new(Arc::clone(&dict), Config::default());
    forgive.import_user(&format!("{pair}@reject\tjia'jia\t甲甲\t100\t{now}\n"));
    for ch in "jiajia".chars() {
        forgive.process(Key::Letter(ch));
    }
    let selected = pick_word(&mut forgive, "甲甲");
    let forgive_export = forgive.export_user();
    push(
        checks,
        "重新选中清除跳过记录",
        selected && !forgive_export.contains("@reject\tjia'jia\t甲甲\t"),
        forgive_export
            .lines()
            .find(|l| l.starts_with("@reject\t"))
            .unwrap_or("无跳过记录")
            .to_string(),
    );

    // Shift+Delete 也要忘掉负分:只导入跳过记录,高亮该词删除后导出不再有它。
    let mut forgotten = Engine::new(Arc::clone(&dict), Config::default());
    forgotten.import_user(&format!("@reject\tni'hao\t你好\t100\t{now}\n"));
    for ch in "nihao".chars() {
        forgotten.process(Key::Letter(ch));
    }
    let index = forgotten
        .candidates()
        .iter()
        .position(|c| c == "你好")
        .unwrap_or(usize::MAX);
    let highlighted = if index == usize::MAX {
        false
    } else {
        forgotten.set_highlight(index);
        forgotten.highlight() == index
    };
    forgotten.process(Key::DeleteCandidate);
    let forgotten_export = forgotten.export_user();
    push(
        checks,
        "Shift+Delete 清除跳过记录",
        highlighted && !forgotten_export.contains("@reject\tni'hao\t你好\t"),
        format!("高亮 {highlighted},导出 {} 行", forgotten_export.lines().count()),
    );

    // 快捷候选不是词:选第二个日期格式时,排在前面的日期不能被记成被跳过的词。
    let mut quick_skip = Engine::new(Arc::clone(&dict), Config::default());
    for ch in "rq".chars() {
        quick_skip.process(Key::Letter(ch));
    }
    let second = quick_skip.candidates().get(1).cloned().unwrap_or_default();
    let picked_quick = pick_word(&mut quick_skip, &second);
    let quick_export = quick_skip.export_user();
    push(
        checks,
        "快捷候选不记跳过",
        picked_quick && !quick_export.contains("@reject\t"),
        format!("上屏 {second:?},导出 {} 行", quick_export.lines().count()),
    );

    // 用户词库上限 2 万条时,按键查询不能退化成每次扫全表。噪声词条刻意避开
    // `ni'hao` 的两个音节与首字母 `n`;目标词条必须仍能排到第一。
    let full_user = Arc::new(RwLock::new(UserDict::new()));
    {
        let syllables = [
            "a", "ba", "ca", "da", "fa", "ga", "ka", "la", "ma", "pa", "qi", "ra", "sa", "ta",
            "wa", "ya", "za", "zha", "cha", "sha",
        ];
        let mut bulk = String::new();
        for i in 0..19_999usize {
            let a = syllables[i % syllables.len()];
            let b = syllables[(i / syllables.len()) % syllables.len()];
            let c = syllables[(i / (syllables.len() * syllables.len())) % syllables.len()];
            let d = syllables[(i / (syllables.len().pow(3))) % syllables.len()];
            bulk.push_str(&format!("{a}'{b}'{c}'{d}\t词{i}\t1\t0\n"));
        }
        bulk.push_str("ni'hao\t尼豪\t100000\t0\n");
        full_user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .import(&bulk);
    }
    let mut large = engine(&dict, &full_user);
    let start = Instant::now();
    let mut top = String::new();
    for _ in 0..5 {
        large.process(Key::Esc);
        for c in "nihao".chars() {
            large.process(Key::Letter(c));
        }
        top = large.candidates().first().cloned().unwrap_or_default();
    }
    let per_key = start.elapsed().as_secs_f64() * 1000.0 / 5.0;
    push(
        checks,
        "大用户词库按形状查询",
        top == "尼豪" && per_key < 100.0,
        format!("{per_key:.1} ms/次,首选 {top}"),
    );
}

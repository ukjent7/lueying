//! 词记忆的 E2E:新词两次选择才首位、误选与跳过减弱、衰减不删词、导入导出往返、
//! 跨会话共享、多音节学习键与私密字段。
//!
//! 时间相关的检查用 `set_sim_now` 注入模拟时钟;检查结束后恢复系统时钟。

use std::sync::RwLock;

use lueying_engine::{Key, UserDict, set_sim_now};

use super::*;

const DAY: u64 = 86_400;
const T0: u64 = 1_800_000_000;

/// 同一个用户词库上的两个会话:一个学习,另一个看结果。
pub(super) fn shared_sessions(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut a = engine(&dict, &user);
    type_keys(&mut a, "xian");
    let second = a.candidates().get(1).cloned().unwrap_or_default();
    for _ in 0..2 {
        type_keys(&mut a, "xian");
        pick_word(&mut a, &second);
    }
    let mut b = engine(&dict, &user);
    type_keys(&mut b, "xian");
    let top = b.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "跨会话共享用户词库",
        !second.is_empty() && top == second,
        format!("学过 {second:?},另一会话首位 {top:?}"),
    );
}

/// 新词第一次选中不升首位,第二次起才升首位。
pub(super) fn two_picks_promote(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut e = engine(&dict, &user);
    type_keys(&mut e, "tianqi");
    let word = e.candidates().get(1).cloned().unwrap_or_default();
    pick_word(&mut e, &word);
    type_keys(&mut e, "tianqi");
    let after_one = e.candidates().first().cloned().unwrap_or_default();
    pick_word(&mut e, &word);
    type_keys(&mut e, "tianqi");
    let after_two = e.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "新词两次选中后首位",
        !word.is_empty() && after_one != word && after_two == word,
        format!("{word:?}:一次后首位 {after_one:?},两次后首位 {after_two:?}"),
    );
}

/// 上屏后立刻删掉重打并改选别的词:误选的词不再占首位。
pub(super) fn mispick_demotes(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut e = engine(&dict, &user);
    type_keys(&mut e, "tianqi");
    let wrong = e.candidates().get(1).cloned().unwrap_or_default();
    for _ in 0..2 {
        type_keys(&mut e, "tianqi");
        pick_word(&mut e, &wrong);
    }
    type_keys(&mut e, "tianqi");
    let before = e.candidates().first().cloned().unwrap_or_default();
    // 立刻删掉重打,改选另一个候选。
    pick_word(&mut e, &before);
    type_keys(&mut e, "tianqi");
    let other = e
        .candidates()
        .iter()
        .find(|c| **c != before)
        .cloned()
        .unwrap_or_default();
    pick_word(&mut e, &other);
    type_keys(&mut e, "tianqi");
    let top = e.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "误选的词不再占首位",
        !before.is_empty() && top != before,
        format!("误选 {before:?},改选 {other:?},之后首位 {top:?}"),
    );
}

/// 同键下反复越过学过的词去选别的词,它的记忆被减弱,最终不再首位。
pub(super) fn skip_weakens(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut e = engine(&dict, &user);
    type_keys(&mut e, "tianqi");
    let learned = e.candidates().get(1).cloned().unwrap_or_default();
    for _ in 0..2 {
        type_keys(&mut e, "tianqi");
        pick_word(&mut e, &learned);
    }
    for _ in 0..2 {
        type_keys(&mut e, "tianqi");
        let other = e
            .candidates()
            .iter()
            .find(|c| **c != learned)
            .cloned()
            .unwrap_or_default();
        pick_word(&mut e, &other);
    }
    type_keys(&mut e, "tianqi");
    let top = e.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "反复越过的词被压下去",
        !learned.is_empty() && top != learned,
        format!("学过 {learned:?},越过两次后首位 {top:?}"),
    );
}

/// 导出再导入逐字相等;坏行整行跳过,不影响好行。
pub(super) fn round_trip_and_bad_lines(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut e = engine(&dict, &user);
    type_keys(&mut e, "tianqi");
    let word = e.candidates().get(1).cloned().unwrap_or_default();
    pick_word(&mut e, &word);
    let exported = e.export_user();
    let mut fresh = UserDict::new();
    fresh.import(&format!("garbage\n{exported}old\t行\t3\t10\n"));
    let again = fresh.export();
    push(
        checks,
        "导出导入往返逐字相等",
        !exported.is_empty() && again == exported,
        format!(
            "导出 {} 行,导入后 {} 行",
            exported.lines().count(),
            again.lines().count()
        ),
    );
}

/// 学过「很多」(hen'duo),敲 hneduo(手误)时,学习加分不能顶上来:手误纠正后的音节与档案键相同,
/// 但用户实际打出的是 hne,学习只认精确读音。整张候选表与完全不学习时逐项相同。
/// 对照:同一档案敲精确全拼 hen'duo,「很多」应当升到首位,证明这份档案确实生效。
pub(super) fn typo_gets_no_bonus(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let fresh = Arc::new(RwLock::new(UserDict::new()));
    let learned = Arc::new(RwLock::new(UserDict::new()));
    for _ in 0..3 {
        learned
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .learn("hen'duo", "很多");
    }
    let without = typed_scores(&dict, &fresh, "hneduo");
    let with = typed_scores(&dict, &learned, "hneduo");
    let exact = typed_scores(&dict, &learned, "hen'duo");
    let exact_top = exact.first().map(|(text, _)| text.as_str());
    let top5: Vec<&str> = exact
        .iter()
        .take(5)
        .map(|(text, _)| text.as_str())
        .collect();
    push(
        checks,
        "手误不加学习分",
        without == with && exact_top == Some("很多"),
        format!(
            "hneduo 候选表与不学习相同: {}; 精确全拼 hen'duo 前五 {top5:?}",
            without == with,
        ),
    );
}

/// 学过单字母简拼「t」→「他」,敲 ting 时「他」只吃掉 t、切进了完整音节 ting 内部,不给学习加分:
/// 整张候选表(文字与分数)与完全不学习时逐项相同。对照:单敲 t 时「他」排首位,档案确实生效。
pub(super) fn abbrev_inside_syllable_no_bonus(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let fresh = Arc::new(RwLock::new(UserDict::new()));
    let learned = Arc::new(RwLock::new(UserDict::new()));
    for _ in 0..3 {
        learned
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .learn("t", "他");
    }
    let without = typed_scores(&dict, &fresh, "ting");
    let with = typed_scores(&dict, &learned, "ting");
    let single = typed_scores(&dict, &learned, "t");
    let single_top = single.first().map(|(text, _)| text.as_str());
    let diff = without
        .iter()
        .zip(&with)
        .position(|(a, b)| a != b)
        .unwrap_or(without.len().min(with.len()));
    push(
        checks,
        "简拼切进完整音节不加学习分",
        without == with && single_top == Some("他"),
        format!(
            "ting 候选表与不学习相同: {}(长度 {} / {};首个不同位置 {diff}: {:?} / {:?}); 单敲 t 首选 {:?}",
            without == with,
            without.len(),
            with.len(),
            without.get(diff),
            with.get(diff),
            single_top
        ),
    );
}

/// 「这不」在 zh'b 下学过。zhebu 是词典确认的读音(zhe'bu),应当提频;zhibu 不是这个词的读音,
/// 互认不能凭空加词:整张候选表(文字与分数)与完全不学习时逐位相同。
pub(super) fn mutual_key_needs_dictionary(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let fresh = Arc::new(RwLock::new(UserDict::new()));
    let learned = Arc::new(RwLock::new(UserDict::new()));
    for _ in 0..3 {
        learned
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .learn("zh'b", "这不");
    }
    let without = typed_scores(&dict, &fresh, "zhibu");
    let with = typed_scores(&dict, &learned, "zhibu");
    let zhebu = typed_scores(&dict, &learned, "zhebu");
    let zhebu_top = zhebu.first().map(|(text, _)| text.as_str());
    let head: Vec<&str> = zhebu
        .iter()
        .take(6)
        .map(|(text, _)| text.as_str())
        .collect();
    let at = zhebu.iter().position(|(text, _)| text == "这不");
    push(
        checks,
        "互认不凭空加词",
        without == with && zhebu_top == Some("这不"),
        format!(
            "zhibu 候选表与不学习相同: {}; zhebu 首选 {:?},这不 位置 {at:?},前六 {head:?}",
            without == with,
            zhebu_top
        ),
    );
}

/// 前缀图复用不能跨过简拼边界的变化:有学习档时,逐键敲入的候选与整串重敲逐位相同。
/// 「t」在 ting 里是否落在边界上取决于后面的字母,复用前缀列时必须算对。
pub(super) fn abbrev_prefix_reuse_matches_fresh(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    {
        let mut guard = user.write().unwrap_or_else(|p| p.into_inner());
        for _ in 0..3 {
            guard.learn("t", "他");
            guard.learn("ting", "厅");
        }
    }
    let mut mismatch = None;
    for input in ["tinghao", "tingting", "tingshi"] {
        let mut incremental = engine(&dict, &user);
        incremental.process(Key::Esc);
        incremental.set_context("");
        let mut typed = String::new();
        for c in input.chars() {
            incremental.process(Key::Letter(c));
            typed.push(c);
            let inc: Vec<(String, u64)> = incremental
                .candidates_detailed()
                .iter()
                .map(|c| (c.text.clone(), c.score.to_bits()))
                .collect();
            if inc != typed_scores(&dict, &user, &typed) && mismatch.is_none() {
                mismatch = Some(typed.clone());
            }
        }
    }
    push(
        checks,
        "前缀复用与整串重敲一致",
        mismatch.is_none(),
        format!("首个不一致的前缀: {mismatch:?}"),
    );
}

/// 原样输入记忆:手误击键串 `hneduo` 上屏的词选两次后升首位。手误不吃读音学习分,
/// 所以升首位只能来自按击键串的记忆。同一档案下,击键串不同的 `hneduos` 候选表与不学习时逐项相同。
pub(super) fn raw_input_memory(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    set_sim_now(T0);
    let mut e = engine(&dict, &user);
    type_keys(&mut e, "hneduo");
    let first = e.candidates().first().cloned().unwrap_or_default();
    let word = e
        .candidates()
        .iter()
        .find(|c| **c != first)
        .cloned()
        .unwrap_or_default();
    for _ in 0..2 {
        type_keys(&mut e, "hneduo");
        pick_word(&mut e, &word);
    }
    type_keys(&mut e, "hneduo");
    let top = e.candidates().first().cloned().unwrap_or_default();
    let fresh = Arc::new(RwLock::new(UserDict::new()));
    let same_other =
        typed_scores(&dict, &user, "hneduos") == typed_scores(&dict, &fresh, "hneduos");
    set_sim_now(0);
    push(
        checks,
        "原样输入记忆",
        !word.is_empty() && top == word && same_other,
        format!("手误 hneduo 选 {word:?} 两次,首位 {top:?};hneduos 候选与不学习相同: {same_other}"),
    );
}

/// 全拼学过「点」(dian)以后,单敲字母 `d` 与不学习时逐位相同:单个字母太模糊,不吃跨形式的学习加分。
pub(super) fn lone_letter_no_cross_bonus(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let fresh = Arc::new(RwLock::new(UserDict::new()));
    let learned = Arc::new(RwLock::new(UserDict::new()));
    for _ in 0..3 {
        learned
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .learn("dian", "点");
    }
    let without = typed_scores(&dict, &fresh, "d");
    let with = typed_scores(&dict, &learned, "d");
    push(
        checks,
        "单字母不吃全拼学习分",
        without == with,
        format!("全拼学过「点」后,d 候选表与不学习相同: {}", without == with),
    );
}

/// 候选表的文字与分数(按位比较),用于确认学习没有改动排序。
fn typed_scores(dict: &Arc<Dictionary>, user: &SharedUser, keys: &str) -> Vec<(String, u64)> {
    let mut e = engine(dict, user);
    type_keys(&mut e, keys);
    e.candidates_detailed()
        .iter()
        .map(|c| (c.text.clone(), c.score.to_bits()))
        .collect()
}

/// 久不用的词只淡化、不删除:闲置很久后仍在候选里。
pub(super) fn decay_keeps_words(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);
    let user = Arc::new(RwLock::new(UserDict::new()));
    set_sim_now(T0);
    let mut e = engine(&dict, &user);
    type_keys(&mut e, "tianqi");
    let word = e.candidates().get(1).cloned().unwrap_or_default();
    for _ in 0..2 {
        type_keys(&mut e, "tianqi");
        pick_word(&mut e, &word);
    }
    set_sim_now(T0 + 1000 * DAY);
    type_keys(&mut e, "tianqi");
    let listed = e.candidates().contains(&word);
    set_sim_now(0);
    push(
        checks,
        "闲置很久的词不删除",
        !word.is_empty() && listed,
        format!("{word:?} 闲置 1000 天后仍在候选中: {listed}"),
    );
}

/// 一条现成的词记忆(导入用):使用次数 `uses`,刚刚使用过。
fn record(key: &str, word: &str, uses: u32) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("w\t{key}\t{word}\t{uses}\t1\t{now}\t0\n")
}

/// 打一段键:上文置空但不清除误选记录(焦点切换才清,见 `clear_context`)。
/// 隔音符是标点键,与字母分开送入(与真实键盘一致)。
fn type_keys(e: &mut lueying_engine::Engine, keys: &str) {
    e.process(Key::Esc);
    e.set_context("");
    for c in keys.chars() {
        if c == '\'' {
            e.process(Key::Punct('\''));
        } else {
            e.process(Key::Letter(c));
        }
    }
}

pub(super) fn learning_keys(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);

    // 多音节词的学习键必须是音节形式(ni'hao);否则新会话按查询键
    // (typed_key 也是 ni'hao)根本找不到学过的词。
    let learned = Arc::new(RwLock::new(UserDict::new()));
    let mut learn = engine(&dict, &learned);
    let mut promoted = false;
    for _ in 0..8 {
        for c in "tianqi".chars() {
            learn.process(Key::Letter(c));
        }
        if learn
            .candidates()
            .first()
            .is_some_and(|c| c.as_str() == "田七")
        {
            promoted = true;
            break;
        }
        if !pick_word(&mut learn, "田七") {
            break;
        }
    }
    let mut replay = engine(&dict, &learned);
    for c in "tianqi".chars() {
        replay.process(Key::Letter(c));
    }
    let top = replay.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "多音节学习键跨会话提频",
        promoted && top == "田七",
        format!("学习后首选 {top}"),
    );

    // 学习时的输入形态与查询形态可以不同:简拼学过换全拼、全拼学过换简拼,
    // 同一个词都要继续提频,否则混用全拼/简拼的用户会觉得「学了没用」。
    for (name, learn_key, typed) in [
        ("简拼学习全拼提频", "t'q", "tianqi"),
        ("全拼学习简拼提频", "tian'qi", "tq"),
    ] {
        let cross = Arc::new(RwLock::new(UserDict::new()));
        engine(&dict, &cross).import_user(&record(learn_key, "田七", 3));
        let mut reader = engine(&dict, &cross);
        for c in typed.chars() {
            reader.process(Key::Letter(c));
        }
        let top = reader.candidates().first().cloned().unwrap_or_default();
        push(
            checks,
            name,
            top == "田七",
            format!("{learn_key} → {typed}:首选 {top}"),
        );
    }

    // 坏行、空键、坏频次都跳过,其余词条照常导入。
    let mut f = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    f.import_user(
        "坏行\nw\tshi\t十\tx\t1\t100\t0\nw\t\tshi\t十\t1\t100\t0\nw\tshi\t十\t4\t1\t100\t0\n",
    );
    let exported = f.export_user();
    let lines: Vec<_> = exported.lines().collect();
    push(
        checks,
        "坏行跳过",
        lines.len() == 1 && lines[0].starts_with("w\tshi\t十\t4\t"),
        format!("{} 条", lines.len()),
    );

    // 分段上屏:选「你」只吃 ni,学习键也应是 ni。
    let mut g = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for ch in "nihao".chars() {
        g.process(Key::Letter(ch));
    }
    g.process(Key::Down);
    let committed: String = g
        .process(Key::Space)
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    let exported = g.export_user();
    push(
        checks,
        "分段上屏只学吃掉的音节",
        committed == "你"
            && exported.contains("w\tni\t你\t")
            && !exported.contains("w\tni'hao\t你\t"),
        format!("上屏 {committed:?}"),
    );

    // 连续分段上屏自动造词:先选「你」再选「好」,整串键与整词一起学。
    let mut phrase =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for ch in "nihao".chars() {
        phrase.process(Key::Letter(ch));
    }
    phrase.process(Key::Down);
    phrase.process(Key::Space);
    phrase.process(Key::Space);
    let exported = phrase.export_user();
    push(
        checks,
        "连续分段上屏自动造词",
        exported.contains("w\tni'hao\t你好\t"),
        exported
            .lines()
            .filter(|l| l.starts_with("w\tni'hao\t"))
            .collect::<Vec<_>>()
            .join(","),
    );

    // 组级适应:连续选两次模糊词后计数应随导出/导入保留。
    let mut group =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for _ in 0..2 {
        for c in "zongguo".chars() {
            group.process(Key::Letter(c));
        }
        group.process(Key::Space);
    }
    let saved = group.export_user();
    let mut restored =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    restored.import_user(&saved);
    let count = restored
        .group_stats()
        .into_iter()
        .find_map(|(name, count)| (name == "zcs").then_some(count))
        .unwrap_or(0);
    push(
        checks,
        "组级学习随用户词库持久化",
        count == 2,
        format!("zcs={count}"),
    );

    // 词条永不删除:导入 20100 条后全部保留。
    let mut h = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    let bulk: String = (0..20_100)
        .map(|i| format!("w\tk{i}\tw{i}\t{}\t1\t100\t0\n", i + 1))
        .collect();
    h.import_user(&bulk);
    let exported = h.export_user();
    let kept = exported.lines().count();
    push(
        checks,
        "词条永不删除",
        kept == 20_100 && exported.contains("w\tk0\tw0\t1\t"),
        format!("保留 {kept} 条"),
    );

    // 私密输入范围:关闭学习后上屏不得写入词库;切回普通字段恢复学习。
    let mut private =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    private.set_learning(false);
    for c in "nihao".chars() {
        private.process(Key::Letter(c));
    }
    private.process(Key::Space);
    let private_export = private.export_user();
    private.set_learning(true);
    for c in "nihao".chars() {
        private.process(Key::Letter(c));
    }
    private.process(Key::Space);
    let public_export = private.export_user();
    push(
        checks,
        "私密字段关闭学习",
        private_export.is_empty() && public_export.contains("w\tni'hao\t你好\t"),
        format!(
            "私密 {} 行,恢复 {} 行",
            private_export.lines().count(),
            public_export.lines().count()
        ),
    );
}

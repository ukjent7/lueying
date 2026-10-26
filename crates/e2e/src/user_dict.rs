//! 用户词库 E2E:先列失败方式,再逐条检查。
//!
//! 1. 选词学习只留在当前会话 → 共用一份词库的两个引擎必须互相看到。
//! 2. 导入重复行重复计分 → 同键同词合并,频次取最大。
//! 3. 导出/导入丢词或坏行连坐 → 往返后词频不变,坏行单独跳过。
//! 4. 上屏不经过学习 → 空格上屏后导出里应出现该键与该词。
//! 5. 用户词压不过词典 → 高频导入词必须排到同键词典词前面。
//! 6. 分段上屏学错键 → 上屏前缀词时只学它吃掉的音节。
//! 7. 长期学习让词库无限膨胀 → 超过上限后按频次/时间淘汰,文件大小有界。
//! 8. 多音节学习键存成原始串 → 新会话必须能用 `ni'hao` 这样的音节键复现提频。
//! 9. 组级适应只留在内存 → 导出/导入后模糊组计数必须还在。
//! 10. 学习时用简拼、查询时用全拼(或反过来)丢掉提频 → 两种输入形态必须互相识别,
//!     Shift+Delete 也要按词删掉所有形态的记录。
//! 11. 用户词库到 2 万条后每个音节组合线性扫全表 → 查询按音节数与首字母收窄,
//!     既不能漏掉该命中的用户词,也不能随词库增长拖慢首键。
//! 12. 上屏只学词频、跨词搭配只留在内存 → 前后字搭配必须随词库一起导出/导入,
//!     新会话能继续用同一份个人上下文。
//! 13. 上下文搭配只写文件、不参与排序 → 学过「最先」后,输入 `xian` 的首选
//!     必须从更高频的「现」切到「先」;标点切断相邻关系时必须退回「现」。
//! 14. 分段上屏后剩余候选在上下文写入前重算 → 下一段必须立刻看到刚上屏的
//!     左邻字,而不是等下一次按键才生效。
//! 15. 词级转移只留在内存 → 「你→好」这类相邻词对必须随词库导出/导入,
//!     新会话继续生效。
//! 16. 词级转移写了却不参与排序 → 上屏「最」后输入 `xian`,应偏向「先」。

use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Event, Key, SharedUser, UserDict};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

fn engine(dict: &Arc<Dictionary>, user: &SharedUser) -> Engine {
    Engine::new(Arc::clone(dict), Config::default()).with_user(Arc::clone(user))
}

/// 翻到候选所在页并点选;用于重复学习同一个词。
fn pick_word(engine: &mut Engine, word: &str) -> bool {
    let Some(index) = engine.candidates().iter().position(|c| c == word) else {
        return false;
    };
    let page_size = engine.page_size().max(1);
    let page = index / page_size;
    while engine.page() < page {
        if !engine.page_by(1) {
            return false;
        }
    }
    engine
        .pick_in_page(index % page_size)
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text.as_str() == word))
}

pub fn run(report: &mut String) -> usize {
    let dict = Arc::new(Dictionary::builtin());
    let mut checks = Vec::new();

    // 上屏路径:整键选词后导出应出现该键与该词。
    let mut e = Engine::new(Arc::clone(&dict), Config::default());
    for c in "nihao".chars() {
        e.process(Key::Letter(c));
    }
    e.process(Key::Space);
    let exported = e.export_user();
    push(
        &mut checks,
        "上屏写入用户词库",
        exported.lines().any(|l| l.starts_with("ni'hao\t你好\t")),
        exported.lines().next().unwrap_or_default().to_string(),
    );
    // 上屏还形成「你→好」的字级搭配;它必须和词条一起持久化。
    let has_context = exported
        .lines()
        .any(|l| l.starts_with("@context\t你\t好\t"));
    push(
        &mut checks,
        "上屏写入上下文搭配",
        has_context,
        exported
            .lines()
            .find(|l| l.starts_with("@context\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let mut roundtrip = Engine::new(Arc::clone(&dict), Config::default());
    roundtrip.import_user(&exported);
    let reexported = roundtrip.export_user();
    push(
        &mut checks,
        "上下文搭配导出导入往返",
        reexported
            .lines()
            .any(|l| l.starts_with("@context\t你\t好\t")),
        reexported
            .lines()
            .find(|l| l.starts_with("@context\t"))
            .unwrap_or_default()
            .to_string(),
    );

    // 字级搭配真正参与排序:导入「最→先」后,输入 `xian` 的首选应从更高频的
    // 「现」切到「先」。时间戳用当前时间,避免导入的数据被判为过期而衰减掉。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let context_line = format!("@context\t最\t先\t100\t{now}\n");
    let mut ranked = Engine::new(Arc::clone(&dict), Config::default());
    ranked.import_user(&context_line);
    ranked.set_context("最");
    for ch in "xian".chars() {
        ranked.process(Key::Letter(ch));
    }
    let biased = ranked.candidates().first().cloned().unwrap_or_default();
    let mut plain = Engine::new(Arc::clone(&dict), Config::default());
    plain.set_context("最");
    for ch in "xian".chars() {
        plain.process(Key::Letter(ch));
    }
    let baseline = plain.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "上下文搭配改变首选",
        baseline == "现" && biased == "先",
        format!("无搭配 {baseline} → 有搭配 {biased}"),
    );

    // 标点把前后字隔开:同样的搭配不能跨过逗号生效。
    let mut cut = Engine::new(Arc::clone(&dict), Config::default());
    cut.import_user(&context_line);
    cut.set_context("最，");
    for ch in "xian".chars() {
        cut.process(Key::Letter(ch));
    }
    let after_punct = cut.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "标点切断上下文搭配",
        after_punct == "现" && after_punct == baseline,
        format!("逗号后首选 {after_punct}"),
    );

    // 分段上屏后剩余拼音立即重算:刚上屏的「好」必须参与下一段排序。
    let segmented_line = format!("@context\t好\t以\t100\t{now}\n");
    let mut segmented = Engine::new(Arc::clone(&dict), Config::default());
    segmented.import_user(&segmented_line);
    for ch in "haoyi".chars() {
        segmented.process(Key::Letter(ch));
    }
    let picked = pick_word(&mut segmented, "好");
    let next_top = segmented.candidates().first().cloned().unwrap_or_default();
    let committed: String = segmented
        .process(Key::Space)
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "分段上屏立即使用上下文",
        picked && next_top == "以" && committed == "以",
        format!("前缀 {picked},下一段首选 {next_top},上屏 {committed:?}"),
    );

    // 词级转移:分段上屏「你」「好」后应记下 你→好,并随词库往返。
    let mut word_flow = Engine::new(Arc::clone(&dict), Config::default());
    for ch in "nihao".chars() {
        word_flow.process(Key::Letter(ch));
    }
    word_flow.process(Key::Down);
    word_flow.process(Key::Space);
    word_flow.process(Key::Space);
    let flow_export = word_flow.export_user();
    push(
        &mut checks,
        "上屏写入词级转移",
        flow_export
            .lines()
            .any(|l| l.starts_with("@transition\t你\t好\t")),
        flow_export
            .lines()
            .find(|l| l.starts_with("@transition\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let mut flow_restored = Engine::new(Arc::clone(&dict), Config::default());
    flow_restored.import_user(&flow_export);
    let flow_reexport = flow_restored.export_user();
    push(
        &mut checks,
        "词级转移导出导入往返",
        flow_reexport
            .lines()
            .any(|l| l.starts_with("@transition\t你\t好\t")),
        flow_reexport
            .lines()
            .find(|l| l.starts_with("@transition\t"))
            .unwrap_or_default()
            .to_string(),
    );

    // 词级转移参与排序:先上屏「最」,再输入 xian,应从「现」偏向「先」。
    let flow_line = format!("zui\t最\t100000\t0\n@transition\t最\t先\t100\t{now}\n");
    let mut flow = Engine::new(Arc::clone(&dict), Config::default());
    flow.import_user(&flow_line);
    for ch in "zui".chars() {
        flow.process(Key::Letter(ch));
    }
    flow.process(Key::Space);
    for ch in "xian".chars() {
        flow.process(Key::Letter(ch));
    }
    let flow_top = flow.candidates().first().cloned().unwrap_or_default();
    let mut no_flow = Engine::new(Arc::clone(&dict), Config::default());
    no_flow.import_user("zui\t最\t100000\t0\n");
    for ch in "zui".chars() {
        no_flow.process(Key::Letter(ch));
    }
    no_flow.process(Key::Space);
    for ch in "xian".chars() {
        no_flow.process(Key::Letter(ch));
    }
    let flow_base = no_flow.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "词级转移改变首选",
        flow_base == "现" && flow_top == "先",
        format!("无转移 {flow_base} → 有转移 {flow_top}"),
    );

    // 两个会话共用一份词库:一边导入,另一边立刻用它排序。
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut a = engine(&dict, &user);
    let mut b = engine(&dict, &user);
    a.import_user("shi\t十\t100000\t0\n");
    for c in "shi".chars() {
        b.process(Key::Letter(c));
    }
    let top = b.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "跨会话共享用户词库",
        top == "十",
        format!("首选 {top}"),
    );

    // 导入合并:同键同词只留一条,频次取最大。
    let mut c = Engine::new(Arc::clone(&dict), Config::default());
    c.import_user("shi\t十\t2\t0\n");
    c.import_user("shi\t十\t3\t0\n");
    let exported = c.export_user();
    let ten: Vec<_> = exported
        .lines()
        .filter(|l| l.starts_with("shi\t十\t"))
        .collect();
    push(
        &mut checks,
        "重复导入同键同词合并",
        ten.len() == 1 && ten[0].starts_with("shi\t十\t3\t"),
        format!("{} 条", ten.len()),
    );

    // 导出再导入(Server 落盘用的就是这份文本),词与频次都要还在。
    let text = a.export_user();
    let mut d = Engine::new(Arc::clone(&dict), Config::default());
    d.import_user(&text);
    for ch in "shi".chars() {
        d.process(Key::Letter(ch));
    }
    let top = d.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "导出导入往返",
        top == "十" && d.export_user().contains("shi\t十\t100000\t"),
        format!("首选 {top}"),
    );

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
        &mut checks,
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
        engine(&dict, &cross).import_user(&format!("{learn_key}\t田七\t100000\t0\n"));
        let mut reader = engine(&dict, &cross);
        for c in typed.chars() {
            reader.process(Key::Letter(c));
        }
        let top = reader.candidates().first().cloned().unwrap_or_default();
        push(
            &mut checks,
            name,
            top == "田七",
            format!("{learn_key} → {typed}:首选 {top}"),
        );
    }

    // 忘掉也要跨输入形态:简拼学过,全拼查询时 Shift+Delete 必须删掉那条记录。
    let forgotten = Arc::new(RwLock::new(UserDict::new()));
    engine(&dict, &forgotten).import_user("t'q\t田七\t100000\t0\n");
    let mut forget = engine(&dict, &forgotten);
    for c in "tianqi".chars() {
        forget.process(Key::Letter(c));
    }
    let before = forget.candidates().first().cloned().unwrap_or_default();
    forget.process(Key::DeleteCandidate);
    let top = forget.candidates().first().cloned().unwrap_or_default();
    let exported = forget.export_user();
    push(
        &mut checks,
        "跨输入形态忘掉学习词",
        before == "田七" && top != "田七" && !exported.contains("\t田七\t"),
        format!("忘掉前 {before},忘掉后首选 {top}"),
    );

    // 坏行、空键、坏频次都跳过,其余词条照常导入。
    let mut f = Engine::new(Arc::clone(&dict), Config::default());
    f.import_user("坏行\nshi\t十\tx\t0\n\tshi\t十\t1\t0\nshi\t十\t4\t0\n");
    let exported = f.export_user();
    let lines: Vec<_> = exported.lines().collect();
    push(
        &mut checks,
        "坏行跳过",
        lines.len() == 1 && lines[0].starts_with("shi\t十\t4\t"),
        format!("{} 条", lines.len()),
    );

    // 分段上屏:选「你」只吃 ni,学习键也应是 ni。
    let mut g = Engine::new(Arc::clone(&dict), Config::default());
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
        &mut checks,
        "分段上屏只学吃掉的音节",
        committed == "你" && exported.contains("ni\t你\t") && !exported.contains("ni'hao\t你\t"),
        format!("上屏 {committed:?}"),
    );

    // 连续分段上屏自动造词:先选「你」再选「好」,整串键与整词一起学。
    let mut phrase = Engine::new(Arc::clone(&dict), Config::default());
    for ch in "nihao".chars() {
        phrase.process(Key::Letter(ch));
    }
    phrase.process(Key::Down);
    phrase.process(Key::Space);
    phrase.process(Key::Space);
    let exported = phrase.export_user();
    push(
        &mut checks,
        "连续分段上屏自动造词",
        exported.contains("ni'hao\t你好\t"),
        exported
            .lines()
            .filter(|l| l.starts_with("ni'hao\t"))
            .collect::<Vec<_>>()
            .join(","),
    );

    // 组级适应:连续选两次模糊词后计数应随导出/导入保留。
    let mut group = Engine::new(Arc::clone(&dict), Config::default());
    for _ in 0..2 {
        for c in "zongguo".chars() {
            group.process(Key::Letter(c));
        }
        group.process(Key::Space);
    }
    let saved = group.export_user();
    let mut restored = Engine::new(Arc::clone(&dict), Config::default());
    restored.import_user(&saved);
    let count = restored
        .group_stats()
        .into_iter()
        .find_map(|(name, count)| (name == "zcs").then_some(count))
        .unwrap_or(0);
    push(
        &mut checks,
        "组级学习随用户词库持久化",
        count == 2,
        format!("zcs={count}"),
    );

    // 设置窗「清空用户词库」:所有会话共用的那份立即变空。
    let mut cleared = Engine::new(Arc::clone(&dict), Config::default());
    cleared.import_user("shi\t十\t9\t0\n");
    cleared.clear_user();
    push(
        &mut checks,
        "清空用户词库",
        cleared.export_user().is_empty(),
        "导出为空".into(),
    );

    // 上限:导入 20100 条后只保留 18000 条,低频先淘汰。
    let mut h = Engine::new(Arc::clone(&dict), Config::default());
    let bulk: String = (0..20_100)
        .map(|i| format!("k{i}\tw{i}\t{}\t0\n", i + 1))
        .collect();
    h.import_user(&bulk);
    let exported = h.export_user();
    let kept = exported.lines().count();
    push(
        &mut checks,
        "用户词库超限淘汰",
        kept == 18_000
            && exported.contains("k20099\tw20099\t20100\t0")
            && !exported.contains("k0\tw0\t1\t0"),
        format!("保留 {kept} 条"),
    );

    // 私密输入范围:关闭学习后上屏不得写入词库;切回普通字段恢复学习。
    let mut private = Engine::new(Arc::clone(&dict), Config::default());
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
        &mut checks,
        "私密字段关闭学习",
        private_export.is_empty() && public_export.contains("ni'hao\t你好\t"),
        format!(
            "私密 {} 行,恢复 {} 行",
            private_export.lines().count(),
            public_export.lines().count()
        ),
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
        &mut checks,
        "大用户词库按形状查询",
        top == "尼豪" && per_key < 100.0,
        format!("{per_key:.1} ms/次,首选 {top}"),
    );

    report.push_str("\n## 用户词库\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0usize;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}

//! 用户词库 E2E:上屏学习、上下文与词序。
//!
//! 这里只放会改变候选排序的学习路径;共享词库与边界条件在别的子模块。

use lueying_engine::Key;

use super::*;

/// E2E:上屏学习、字级上下文、分段上屏与词序/三词窗口的排序。
pub(super) fn commit_and_context(shared: &Arc<Dictionary>, checks: &mut Checks, now: u64) {
    let dict = Arc::clone(shared);

    // 上屏路径:整键选词后导出应出现该键与该词。
    let mut e = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for c in "nihao".chars() {
        e.process(Key::Letter(c));
    }
    e.process(Key::Space);
    let exported = e.export_user();
    push(
        checks,
        "上屏写入用户词库",
        exported.lines().any(|l| l.starts_with("ni'hao\t你好\t")),
        exported.lines().next().unwrap_or_default().to_string(),
    );
    // 上屏还形成「你→好」的字级搭配;它必须和词条一起持久化。
    let has_context = exported
        .lines()
        .any(|l| l.starts_with("@context\t你\t好\t"));
    push(
        checks,
        "上屏写入上下文搭配",
        has_context,
        exported
            .lines()
            .find(|l| l.starts_with("@context\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let mut roundtrip =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    roundtrip.import_user(&exported);
    let reexported = roundtrip.export_user();
    push(
        checks,
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
    let context_line = format!("@context\t最\t先\t100\t{now}\n");
    let mut ranked =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    ranked.import_user(&context_line);
    ranked.set_context("最");
    for ch in "xian".chars() {
        ranked.process(Key::Letter(ch));
    }
    let biased = ranked.candidates().first().cloned().unwrap_or_default();
    let mut plain =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    plain.set_context("最");
    for ch in "xian".chars() {
        plain.process(Key::Letter(ch));
    }
    let baseline = plain.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "上下文搭配改变首选",
        baseline == "现" && biased == "先",
        format!("无搭配 {baseline} → 有搭配 {biased}"),
    );

    // 标点把前后字隔开:同样的搭配不能跨过逗号生效。
    let mut cut = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    cut.import_user(&context_line);
    cut.set_context("最，");
    for ch in "xian".chars() {
        cut.process(Key::Letter(ch));
    }
    let after_punct = cut.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "标点切断上下文搭配",
        after_punct == "现" && after_punct == baseline,
        format!("逗号后首选 {after_punct}"),
    );

    // 分段上屏后剩余拼音立即重算:刚上屏的「好」必须参与下一段排序。
    let segmented_line = format!("@context\t好\t以\t100\t{now}\n");
    let mut segmented =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
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
        checks,
        "分段上屏立即使用上下文",
        picked && next_top == "以" && committed == "以",
        format!("前缀 {picked},下一段首选 {next_top},上屏 {committed:?}"),
    );
}

/// E2E:词级转移与三词窗口的写入、往返和排序。
pub(super) fn word_sequences(shared: &Arc<Dictionary>, checks: &mut Checks, now: u64) {
    let dict = Arc::clone(shared);

    // 词级转移:分段上屏「你」「好」后应记下 你→好,并随词库往返。
    let mut word_flow =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for ch in "nihao".chars() {
        word_flow.process(Key::Letter(ch));
    }
    word_flow.process(Key::Down);
    word_flow.process(Key::Space);
    word_flow.process(Key::Space);
    let flow_export = word_flow.export_user();
    push(
        checks,
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
    let mut flow_restored =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    flow_restored.import_user(&flow_export);
    let flow_reexport = flow_restored.export_user();
    push(
        checks,
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
    let mut flow =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    flow.import_user(&flow_line);
    for ch in "zui".chars() {
        flow.process(Key::Letter(ch));
    }
    flow.process(Key::Space);
    for ch in "xian".chars() {
        flow.process(Key::Letter(ch));
    }
    let flow_top = flow.candidates().first().cloned().unwrap_or_default();
    let mut no_flow =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
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
        checks,
        "词级转移改变首选",
        flow_base == "现" && flow_top == "先",
        format!("无转移 {flow_base} → 有转移 {flow_top}"),
    );

    // 三词窗口:分段上屏「你」「好」后选「先」,应记下 你/好→先 并随词库往返。
    let mut tri_flow =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for ch in "nihao".chars() {
        tri_flow.process(Key::Letter(ch));
    }
    tri_flow.process(Key::Down);
    tri_flow.process(Key::Space);
    tri_flow.process(Key::Space);
    for ch in "xian".chars() {
        tri_flow.process(Key::Letter(ch));
    }
    let tri_picked = pick_word(&mut tri_flow, "先");
    let tri_export = tri_flow.export_user();
    push(
        checks,
        "上屏写入三词窗口",
        tri_picked
            && tri_export
                .lines()
                .any(|l| l.starts_with("@trigram\t你\t好\t先\t")),
        tri_export
            .lines()
            .find(|l| l.starts_with("@trigram\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let mut tri_restored =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    tri_restored.import_user(&tri_export);
    let tri_reexport = tri_restored.export_user();
    push(
        checks,
        "三词窗口导出导入往返",
        tri_reexport
            .lines()
            .any(|l| l.starts_with("@trigram\t你\t好\t先\t")),
        tri_reexport
            .lines()
            .find(|l| l.starts_with("@trigram\t"))
            .unwrap_or_default()
            .to_string(),
    );

    // 三词窗口参与排序:先上屏「你」「好」,再输入 xian,应从「现」偏向「先」。
    let tri_line = format!("@trigram\t你\t好\t先\t100\t{now}\n");
    let seed = |engine: &mut Engine| {
        for ch in "nihao".chars() {
            engine.process(Key::Letter(ch));
        }
        engine.process(Key::Down);
        engine.process(Key::Space);
        engine.process(Key::Space);
    };
    let mut tri_ranked =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    tri_ranked.import_user(&tri_line);
    seed(&mut tri_ranked);
    for ch in "xian".chars() {
        tri_ranked.process(Key::Letter(ch));
    }
    let tri_top = tri_ranked.candidates().first().cloned().unwrap_or_default();
    let mut tri_plain =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    seed(&mut tri_plain);
    for ch in "xian".chars() {
        tri_plain.process(Key::Letter(ch));
    }
    let tri_base = tri_plain.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "三词窗口改变首选",
        tri_base == "现" && tri_top == "先",
        format!("无三词 {tri_base} → 有三词 {tri_top}"),
    );

    // 标点切断词序:同样的三词窗口不能跨过逗号生效。
    let mut tri_cut =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    tri_cut.import_user(&tri_line);
    seed(&mut tri_cut);
    tri_cut.process(Key::Punct(','));
    for ch in "xian".chars() {
        tri_cut.process(Key::Letter(ch));
    }
    let tri_after = tri_cut.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "标点切断三词窗口",
        tri_after == "现" && tri_after == tri_base,
        format!("逗号后首选 {tri_after}"),
    );
}

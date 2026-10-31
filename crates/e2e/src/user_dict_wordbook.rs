//! 用户词库 E2E:共享、持久化与学习键形态。
//!
//! 覆盖跨会话共享、导入合并/往返、应用偏好、学习键归一、上限与私密输入。

use std::sync::RwLock;

use lueying_engine::{Key, UserDict};

use super::*;

/// E2E:候选窗右键的置顶与降权(引擎语义;菜单 UI 走同一请求通道)。
pub(super) fn pin_and_demote(shared: &Arc<Dictionary>, checks: &mut Checks) {
    let dict = Arc::clone(shared);

    // 置顶:把页内第二条钉到首位;导出往返与新会话都要保住。
    let mut e = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for ch in "tianqi".chars() {
        e.process(Key::Letter(ch));
    }
    let list = e.candidates();
    let Some(second) = list.get(1).cloned() else {
        push(checks, "置顶候选充足", false, format!("{list:?}"));
        return;
    };
    push(
        checks,
        "置顶候选充足",
        !second.is_empty() && list.first().map(String::as_str) != Some(second.as_str()),
        format!("{list:?}"),
    );
    e.pin_in_page(1);
    push(
        checks,
        "置顶立即首位",
        e.candidates().first().map(String::as_str) == Some(second.as_str()),
        format!("{:?}", e.candidates()),
    );
    let exported = e.export_user();
    push(
        checks,
        "置顶写进词库",
        exported.lines().any(|l| l.starts_with("@pin\t")),
        exported
            .lines()
            .find(|l| l.starts_with("@pin\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let mut restored =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    restored.import_user(&exported);
    for ch in "tianqi".chars() {
        restored.process(Key::Letter(ch));
    }
    push(
        checks,
        "置顶跨会话首位",
        restored.candidates().first().map(String::as_str) == Some(second.as_str()),
        format!("{:?}", restored.candidates()),
    );

    // Shift+Delete:置顶连同学习记录一起取消,回到词典顺序。
    let mut f = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    f.import_user(&exported);
    for ch in "tianqi".chars() {
        f.process(Key::Letter(ch));
    }
    f.process(Key::DeleteCandidate);
    let cleared = f.export_user();
    push(
        checks,
        "删除取消置顶",
        !cleared.lines().any(|l| l.starts_with("@pin\t"))
            && f.candidates().first().map(String::as_str) == list.first().map(String::as_str),
        format!("{:?} 原首位 {:?}", f.candidates().first(), list.first()),
    );

    // 降权:第一条记一次跳过负分,词库出现 @reject 行。
    let mut d = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    for ch in "tianqi".chars() {
        d.process(Key::Letter(ch));
    }
    let first = d.candidates().first().cloned().unwrap_or_default();
    d.demote_in_page(0);
    let exported = d.export_user();
    push(
        checks,
        "降权记负分",
        exported.lines().any(|l| l.starts_with("@reject\t")),
        exported
            .lines()
            .find(|l| l.starts_with("@reject\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let after = d.candidates();
    push(
        checks,
        "降权后首选变化",
        after.first().map(String::as_str) != Some(first.as_str()),
        format!("{first} → {after:?}"),
    );
}

/// E2E:按应用偏好、跨会话共享、导入合并与往返。
pub(super) fn app_and_shared(shared: &Arc<Dictionary>, checks: &mut Checks, now: u64) {
    let dict = Arc::clone(shared);

    // 按应用偏好:Code.exe 里选「先」后应记下宿主(小写),并随词库往返。
    let mut app_flow =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    app_flow.set_host("Code.exe");
    for ch in "xian".chars() {
        app_flow.process(Key::Letter(ch));
    }
    let app_picked = pick_word(&mut app_flow, "先");
    let app_export = app_flow.export_user();
    push(
        checks,
        "上屏写入应用偏好",
        app_picked
            && app_export
                .lines()
                .any(|l| l.starts_with("@app\tcode.exe\t先\t")),
        app_export
            .lines()
            .find(|l| l.starts_with("@app\t"))
            .unwrap_or_default()
            .to_string(),
    );
    let mut app_restored =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    app_restored.import_user(&app_export);
    let app_reexport = app_restored.export_user();
    push(
        checks,
        "应用偏好导出导入往返",
        app_reexport
            .lines()
            .any(|l| l.starts_with("@app\tcode.exe\t先\t")),
        app_reexport
            .lines()
            .find(|l| l.starts_with("@app\t"))
            .unwrap_or_default()
            .to_string(),
    );

    // 应用偏好参与排序:同一份词库在 Code.exe 下偏向「先」,换宿主或无宿主回到「现」。
    // 导入行故意用大写宿主名,验证导入与查询都会归一到小写。
    let app_line = format!("@app\tCODE.EXE\t先\t100\t{now}\n");
    let mut app_code =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    app_code.import_user(&app_line);
    app_code.set_host("Code.exe");
    for ch in "xian".chars() {
        app_code.process(Key::Letter(ch));
    }
    let app_top = app_code.candidates().first().cloned().unwrap_or_default();
    let mut app_chrome =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    app_chrome.import_user(&app_line);
    app_chrome.set_host("chrome.exe");
    for ch in "xian".chars() {
        app_chrome.process(Key::Letter(ch));
    }
    let app_other = app_chrome.candidates().first().cloned().unwrap_or_default();
    let mut app_none =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    app_none.import_user(&app_line);
    for ch in "xian".chars() {
        app_none.process(Key::Letter(ch));
    }
    let app_plain = app_none.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "应用偏好改变首选",
        app_top == "先" && app_other == "现" && app_plain == "现",
        format!("Code {app_top} / chrome {app_other} / 无宿主 {app_plain}"),
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
        checks,
        "跨会话共享用户词库",
        top == "十",
        format!("首选 {top}"),
    );

    // 导入合并:同键同词只留一条,频次取最大。
    let mut c = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    c.import_user("shi\t十\t2\t0\n");
    c.import_user("shi\t十\t3\t0\n");
    let exported = c.export_user();
    let ten: Vec<_> = exported
        .lines()
        .filter(|l| l.starts_with("shi\t十\t"))
        .collect();
    push(
        checks,
        "重复导入同键同词合并",
        ten.len() == 1 && ten[0].starts_with("shi\t十\t3\t"),
        format!("{} 条", ten.len()),
    );

    // 导出再导入(Server 落盘用的就是这份文本),词与频次都要还在。
    let text = a.export_user();
    let mut d = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    d.import_user(&text);
    for ch in "shi".chars() {
        d.process(Key::Letter(ch));
    }
    let top = d.candidates().first().cloned().unwrap_or_default();
    push(
        checks,
        "导出导入往返",
        top == "十" && d.export_user().contains("shi\t十\t100000\t"),
        format!("首选 {top}"),
    );
}

/// E2E:多音节/跨形态学习键、组级适应、上限、私密输入与清空。
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
        engine(&dict, &cross).import_user(&format!("{learn_key}\t田七\t100000\t0\n"));
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
        checks,
        "跨输入形态忘掉学习词",
        before == "田七" && top != "田七" && !exported.contains("\t田七\t"),
        format!("忘掉前 {before},忘掉后首选 {top}"),
    );

    // 坏行、空键、坏频次都跳过,其余词条照常导入。
    let mut f = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    f.import_user("坏行\nshi\t十\tx\t0\n\tshi\t十\t1\t0\nshi\t十\t4\t0\n");
    let exported = f.export_user();
    let lines: Vec<_> = exported.lines().collect();
    push(
        checks,
        "坏行跳过",
        lines.len() == 1 && lines[0].starts_with("shi\t十\t4\t"),
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
        committed == "你" && exported.contains("ni\t你\t") && !exported.contains("ni'hao\t你\t"),
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
        exported.contains("ni'hao\t你好\t"),
        exported
            .lines()
            .filter(|l| l.starts_with("ni'hao\t"))
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

    // 设置窗「清空用户词库」:所有会话共用的那份立即变空。
    let mut cleared =
        Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    cleared.import_user("shi\t十\t9\t0\n");
    cleared.clear_user();
    push(
        checks,
        "清空用户词库",
        cleared.export_user().is_empty(),
        "导出为空".into(),
    );

    // 上限:导入 20100 条后只保留 18000 条,低频先淘汰。
    let mut h = Engine::new(Arc::clone(&dict), Config::default()).with_modules(crate::builtins());
    let bulk: String = (0..20_100)
        .map(|i| format!("k{i}\tw{i}\t{}\t0\n", i + 1))
        .collect();
    h.import_user(&bulk);
    let exported = h.export_user();
    let kept = exported.lines().count();
    push(
        checks,
        "用户词库超限淘汰",
        kept == 18_000
            && exported.contains("k20099\tw20099\t20100\t0")
            && !exported.contains("k0\tw0\t1\t0"),
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
        private_export.is_empty() && public_export.contains("ni'hao\t你好\t"),
        format!(
            "私密 {} 行,恢复 {} 行",
            private_export.lines().count(),
            public_export.lines().count()
        ),
    );
}

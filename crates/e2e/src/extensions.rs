//! 扩展 E2E:双拼、跨方案学习、自定义短语、辅助码与下一词联想。
//!
//! 失败方式:
//! 1. 双拼展开错位 → 候选不是目标词,或回删/上屏切错击键。
//! 2. 学习键仍用原始击键 → 换方案后同一个词学不到。
//! 3. 自定义短语没有进入候选,或辅码没有筛掉字形不符的候选。
//! 4. 联想没有从本机词序里取到下一词,或把辅码组合搅乱。

use std::sync::{Arc, RwLock};

use lueying_dict::Dictionary;
use lueying_engine::module::Modules;
use lueying_engine::scheme::Scheme;
use lueying_engine::{Config, Engine, Event, Key, SharedUser, UserDict};
use lueying_features::{AuxCode, CustomPhrases, SchemeId};

fn engine(scheme: SchemeId, aux: bool, modules: Arc<Modules>, user: SharedUser) -> Engine {
    Engine::new(Arc::new(Dictionary::builtin()), Config::default())
        .with_user(user)
        .with_modules(modules)
        .with_scheme(lueying_features::scheme(scheme, aux))
}

fn shared() -> SharedUser {
    Arc::new(RwLock::new(UserDict::new()))
}

fn type_text(engine: &mut Engine, text: &str) {
    for c in text.chars() {
        match c {
            'a'..='z' => {
                engine.process(Key::Letter(c));
            }
            ';' | '`' => {
                engine.process(Key::Punct(c));
            }
            _ => {}
        }
    }
}

fn first(engine: &Engine) -> String {
    engine.candidates().first().cloned().unwrap_or_default()
}

fn has(engine: &Engine, word: &str) -> bool {
    engine.candidates().iter().any(|text| text.as_str() == word)
}

fn commit(engine: &mut Engine, word: &str) -> bool {
    let Some(index) = engine.candidates().iter().position(|text| text == word) else {
        return false;
    };
    engine
        .pick(index)
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text == word))
}

fn check(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 小鹤:ni + hc = 你好。自然码/微软/搜狗用 hk = 好。
    for (id, keys) in [
        (SchemeId::Xiaohe, "nihc"),
        (SchemeId::Ziranma, "nihk"),
        (SchemeId::Mspy, "nihk"),
        (SchemeId::Sogou, "nihk"),
    ] {
        let mut e = engine(id, false, Arc::new(Modules::new()), shared());
        type_text(&mut e, keys);
        check(
            &mut checks,
            "双拼整词",
            first(&e) == "你好",
            format!("{} {keys} → {:?}", id.name(), e.candidates()),
        );
        let committed = e
            .process(Key::Space)
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "你好"));
        check(
            &mut checks,
            "双拼上屏",
            committed && e.composition().is_none(),
            format!("{} 空格后组合 = {:?}", id.name(), e.composition()),
        );
    }

    // 双拼两键就是一个音节:零声母与相邻音节不能靠核心再猜一次切分。
    let xiaohe = lueying_features::scheme(SchemeId::Xiaohe, false);
    check(
        &mut checks,
        "双拼零声母",
        xiaohe.expand("aa").query == "a" && xiaohe.expand("ai").query == "ai",
        format!(
            "aa → {}, ai → {}",
            xiaohe.expand("aa").query,
            xiaohe.expand("ai").query
        ),
    );
    check(
        &mut checks,
        "双拼音节边界",
        xiaohe.expand("xian").query == "xi'an" && xiaohe.expand("xm").query == "xian",
        format!(
            "xian → {}, xm → {}",
            xiaohe.expand("xian").query,
            xiaohe.expand("xm").query
        ),
    );

    // 微软/搜狗的 `;` 是 ing,不是标点。
    let mspy = lueying_features::scheme(SchemeId::Mspy, false);
    let expanded = mspy.expand("y;");
    check(
        &mut checks,
        "双拼符号韵母",
        expanded.query == "ying",
        format!("mspy y; → {}", expanded.query),
    );
    let mut semicolon = engine(SchemeId::Mspy, false, Arc::new(Modules::new()), shared());
    type_text(&mut semicolon, "y;");
    check(
        &mut checks,
        "双拼符号键进组合",
        semicolon.composition().is_some_and(|comp| comp.raw == "y;"),
        format!("组合 = {:?}", semicolon.composition().map(|c| &c.raw)),
    );

    // 跨方案学习:小鹤选过的词,全拼同一按键应标记为已学。
    let user = shared();
    let mut xiaohe = engine(
        SchemeId::Xiaohe,
        false,
        Arc::new(Modules::new()),
        Arc::clone(&user),
    );
    type_text(&mut xiaohe, "nihc");
    check(
        &mut checks,
        "双拼学习写入",
        commit(&mut xiaohe, "你好"),
        format!("候选 = {:?}", xiaohe.candidates()),
    );
    let mut full = engine(
        SchemeId::Full,
        false,
        Arc::new(Modules::new()),
        Arc::clone(&user),
    );
    type_text(&mut full, "nihao");
    let learned = full
        .candidates_detailed()
        .first()
        .is_some_and(|cand| cand.text == "你好" && cand.learned);
    check(
        &mut checks,
        "跨方案学习",
        learned,
        format!(
            "全拼首选 = {:?}, key = {:?}",
            full.candidates_detailed().first().map(|c| &c.text),
            user.read()
                .unwrap_or_else(|p| p.into_inner())
                .key_for_word("你好")
        ),
    );

    // 置顶键同样用规范全拼:小鹤置顶的词,全拼同音也应直接排首位。
    let pin_user = shared();
    let mut pin_xiaohe = engine(
        SchemeId::Xiaohe,
        false,
        Arc::new(Modules::new()),
        Arc::clone(&pin_user),
    );
    type_text(&mut pin_xiaohe, "nihc");
    let pinned = pin_xiaohe.pin(0);
    let mut pin_full = engine(
        SchemeId::Full,
        false,
        Arc::new(Modules::new()),
        Arc::clone(&pin_user),
    );
    type_text(&mut pin_full, "nihao");
    check(
        &mut checks,
        "跨方案置顶",
        pinned && first(&pin_full) == "你好",
        format!("pin={pinned}, 全拼首选 = {:?}", pin_full.candidates()),
    );

    // 运行中切换方案:旧击键不能按新规则继续解释。
    let mut switching = engine(SchemeId::Full, false, Arc::new(Modules::new()), shared());
    type_text(&mut switching, "ni");
    switching.set_scheme(lueying_features::scheme(SchemeId::Xiaohe, false));
    check(
        &mut checks,
        "切换方案清组合",
        switching.composition().is_none(),
        format!("组合 = {:?}", switching.composition()),
    );

    // 自定义短语:全拼与双拼都按规范全拼匹配。
    let phrases = Arc::new(CustomPhrases::from_text(
        "你好\tni'hao\t100\n我的邮箱\two'de'you'xiang\t50\n坏行\n",
    ));
    check(
        &mut checks,
        "短语坏行跳过",
        phrases.len() == 2,
        format!("{} 条", phrases.len()),
    );
    let modules = lueying_features::registry(Some(Arc::clone(&phrases)));
    let mut full = engine(SchemeId::Full, false, modules.clone(), shared());
    type_text(&mut full, "nihao");
    check(
        &mut checks,
        "自定义短语全拼",
        first(&full) == "你好"
            && full
                .candidates_detailed()
                .first()
                .and_then(|c| c.hint.as_deref())
                == Some("自定义短语"),
        format!("候选 = {:?}", full.candidates()),
    );
    let mut xiaohe = engine(SchemeId::Xiaohe, false, modules.clone(), shared());
    type_text(&mut xiaohe, "nihc");
    check(
        &mut checks,
        "自定义短语双拼",
        first(&xiaohe) == "你好",
        format!("候选 = {:?}", xiaohe.candidates()),
    );
    // 编码带隔音符时同时建立简拼索引;全拼方案的 `nh` 也要命中。
    let mut initials = engine(SchemeId::Full, false, modules, shared());
    type_text(&mut initials, "nh");
    check(
        &mut checks,
        "自定义短语简拼",
        first(&initials) == "你好",
        format!("候选 = {:?}", initials.candidates()),
    );

    // 辅助码:shi + `j 只留五笔码以 j 开头的字(是/时),十(fg)应被筛掉。
    let mut plain = engine(SchemeId::Full, false, Arc::new(Modules::new()), shared());
    type_text(&mut plain, "shi");
    check(
        &mut checks,
        "辅助码对照",
        has(&plain, "十"),
        format!("未开辅码 = {:?}", plain.candidates()),
    );
    let mut aux = engine(SchemeId::Full, true, Arc::new(Modules::new()), shared());
    type_text(&mut aux, "shi`jg");
    let filtered = !has(&aux, "十") && has(&aux, "是");
    check(
        &mut checks,
        "辅助码筛选",
        filtered,
        format!("shi`jg = {:?}", aux.candidates()),
    );
    let committed = aux
        .process(Key::Space)
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text == "是"));
    check(
        &mut checks,
        "辅助码上屏",
        committed && aux.composition().is_none(),
        format!("空格后组合 = {:?}", aux.composition()),
    );
    check(
        &mut checks,
        "内置辅码表",
        AuxCode::builtin().len() > 7_000,
        format!("{} 字", AuxCode::builtin().len()),
    );
    // `az` 在码表里没有任何前缀命中;应退回完整候选,不能把输入卡死。
    let mut fallback = engine(SchemeId::Full, true, Arc::new(Modules::new()), shared());
    type_text(&mut fallback, "shi`az");
    check(
        &mut checks,
        "辅码无匹配回退",
        has(&fallback, "十") && !fallback.candidates().is_empty(),
        format!("shi`az = {:?}", fallback.candidates()),
    );

    // 联想:先形成「我 → 学习」的词序,再在上一词是「我」时打 x。
    let user = shared();
    let prediction = Arc::new(lueying_features::Prediction::new());
    let modules = lueying_features::registry_with_prediction(None, Arc::clone(&prediction));
    let mut train = engine(SchemeId::Full, false, modules.clone(), Arc::clone(&user));
    type_text(&mut train, "wo");
    let got_wo = commit(&mut train, "我");
    type_text(&mut train, "xuexi");
    let got_xue = commit(&mut train, "学习");
    type_text(&mut train, "wo");
    let again = commit(&mut train, "我");
    type_text(&mut train, "x");
    let suggested = train
        .candidates_detailed()
        .iter()
        .any(|cand| cand.text == "学习" && cand.hint.as_deref() == Some("联想"));
    check(
        &mut checks,
        "下一词联想",
        got_wo && got_xue && again && suggested,
        format!(
            "训练={got_wo}/{got_xue}/{again}, 候选 = {:?}, key = {:?}",
            train.candidates(),
            user.read()
                .unwrap_or_else(|p| p.into_inner())
                .key_for_word("学习")
        ),
    );
    // 设置页关掉联想后,同一份词序不应再补候选。
    prediction.set_enabled(false);
    let mut off = engine(SchemeId::Full, false, modules, Arc::clone(&user));
    type_text(&mut off, "wo");
    let _ = commit(&mut off, "我");
    type_text(&mut off, "x");
    check(
        &mut checks,
        "联想开关",
        !off.candidates_detailed()
            .iter()
            .any(|cand| cand.text == "学习" && cand.hint.as_deref() == Some("联想")),
        format!("关闭后候选 = {:?}", off.candidates()),
    );

    // SDK 注册表:模块声明与方案声明可被宿主枚举。
    let manifests: Vec<&str> = phrases_manifests(&phrases);
    check(
        &mut checks,
        "扩展清单",
        manifests.contains(&"custom-phrases") && manifests.contains(&"prediction"),
        format!("{manifests:?}"),
    );
    let aux_manifest = lueying_features::scheme(SchemeId::Xiaohe, true).manifest();
    check(
        &mut checks,
        "方案叠加",
        aux_manifest.id == "xiaohe-aux",
        format!("{} / {}", aux_manifest.id, aux_manifest.name),
    );

    report
        .push_str("\n## 扩展:双拼 / 短语 / 辅码 / 联想\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
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

fn phrases_manifests(phrases: &Arc<CustomPhrases>) -> Vec<&'static str> {
    lueying_features::registry(Some(Arc::clone(phrases)))
        .manifests()
        .map(|manifest| manifest.id)
        .collect()
}

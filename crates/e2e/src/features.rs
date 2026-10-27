//! 核心输入补齐 E2E:符号、emoji、简繁与英文混输。
//!
//! 先列失败方式:
//! 1. v 模式只认数字,`vjt` / `v/jt` 打不出箭头等符号。
//! 2. 符号代码抢掉 `v123`、`v1+2` 与四位年份日期。
//! 3. 符号候选写进用户词库,或在重排时被换走。
//! 4. 表情只按码点输入,常用词后不跟随表情候选。
//! 5. 表情候选抢掉词本身的首选,或上屏后学成用户词。
//! 6. 单字候选后插表情,把第二个同音字的数字序号挤走。
//! 7. 英文候选打断半截拼音:输入 `nih` 时把原文顶到候选首位。
//! 8. 英文大小写丢失,`iphone` 只能上屏小写;`vscode` 被 v 快捷前缀吞掉。
//! 9. 简繁只做单字映射:头发/干净/皇后/面条等歧义词转换错。
//! 10. 繁体候选写进词库,切回简体后仍召回繁体词。
//! 11. 组合中切换简繁不重算,内联显示与候选仍是旧字形。
//! 12. 繁体模式误转英文、拼音原文或快捷输入的 ASCII 部分。

use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Event, Key};

const TEST_DICT: &str = "\
微笑\t1000000\twei'xiao
头发\t1000000\ttou'fa
干净\t1000000\tgan'jing
干杯\t1000000\tgan'bei
皇后\t1000000\thuang'hou
后来\t1000000\thou'lai
里面\t1000000\tli'mian
制造\t1000000\tzhi'zao
一只\t1000000\tyi'zhi
台湾\t1000000\ttai'wan
云端\t1000000\tyun'duan
丑陋\t1000000\tchou'lou
发展\t1000000\tfa'zhan
后台\t1000000\thou'tai
面条\t1000000\tmian'tiao
";

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

fn custom(traditional: bool) -> Engine {
    let (dict, bad) = Dictionary::from_text(TEST_DICT);
    assert_eq!(bad, 0, "测试词库不应有坏行");
    Engine::new(
        Arc::new(dict),
        Config {
            traditional,
            ..Config::default()
        },
    )
}

fn mini() -> Engine {
    Engine::new(Arc::new(Dictionary::builtin()), Config::default())
}

fn letters(engine: &mut Engine, text: &str) {
    for c in text.chars() {
        engine.process(Key::Letter(c));
    }
}

/// `v` + 分类代码;`slash` 为真时走 `v/代码` 写法。
fn symbols(engine: &mut Engine, code: &str, slash: bool) {
    engine.process(Key::Letter('v'));
    if slash {
        engine.process(Key::Punct('/'));
    }
    for c in code.chars() {
        engine.process(Key::Letter(c));
    }
}

fn top(engine: &Engine) -> String {
    engine.candidates().first().cloned().unwrap_or_default()
}

fn display(engine: &Engine) -> String {
    engine
        .composition()
        .map(|comp| comp.display.clone())
        .unwrap_or_default()
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 符号:斜杠写法与直接写法都能出分类表;首个符号是该分类的第一项。
    let mut slash = mini();
    symbols(&mut slash, "jt", true);
    let list = slash.candidates();
    let hint = slash
        .candidates_detailed()
        .first()
        .and_then(|cand| cand.hint.as_deref());
    push(
        &mut checks,
        "v/符号代码",
        top(&slash) == "↑" && list.iter().any(|text| text == "→") && hint == Some("箭头"),
        format!("{list:?} 提示 {hint:?}"),
    );

    let mut direct = mini();
    symbols(&mut direct, "bd", false);
    let list = direct.candidates();
    push(
        &mut checks,
        "v 直接符号代码",
        top(&direct) == "、"
            && list.iter().any(|text| text == "。")
            && direct.composition().map(|comp| comp.raw.as_str()) == Some("vbd"),
        format!("{list:?}"),
    );

    // 原有 v 数字/算式不能被符号模式抢走。
    let mut number = mini();
    number.process(Key::Letter('v'));
    for d in [1, 2, 3] {
        number.process(Key::Digit(d));
    }
    push(
        &mut checks,
        "符号不抢 v 数字",
        top(&number) == "一百二十三",
        format!("{:?}", number.candidates()),
    );

    let mut expression = mini();
    expression.process(Key::Letter('v'));
    expression.process(Key::Digit(1));
    expression.process(Key::Punct('+'));
    expression.process(Key::Digit(2));
    push(
        &mut checks,
        "符号不抢 v 算式",
        top(&expression) == "3",
        format!("{:?}", expression.candidates()),
    );

    let mut symbol_flow = mini();
    symbols(&mut symbol_flow, "bd", false);
    let events = symbol_flow.process(Key::Space);
    let exported = symbol_flow.export_user();
    push(
        &mut checks,
        "符号上屏不学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "、"))
            && symbol_flow.composition().is_none()
            && exported.is_empty(),
        format!("{events:?} 词库 {} 行", exported.lines().count()),
    );

    // 表情:词候选后紧跟表情,不抢词本身的首选;选中表情不写词库。
    let mut emoji = custom(false);
    letters(&mut emoji, "weixiao");
    let list = emoji.candidates();
    let emoji_at = list.iter().position(|text| text == "😊");
    let emoji_hint = emoji_at
        .and_then(|index| emoji.candidates_detailed().get(index))
        .and_then(|cand| cand.hint.as_deref());
    push(
        &mut checks,
        "词后表情候选",
        top(&emoji) == "微笑" && emoji_at == Some(1) && emoji_hint == Some("表情"),
        format!("{list:?} 提示 {emoji_hint:?}"),
    );
    let events = emoji.process(Key::Digit(2));
    let exported = emoji.export_user();
    push(
        &mut checks,
        "表情上屏不学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "😊"))
            && exported.is_empty(),
        format!("{events:?} 词库 {} 行", exported.lines().count()),
    );

    let mut promoted = custom(false);
    letters(&mut promoted, "weixiao");
    let emoji_at = promoted.candidates().iter().position(|text| text == "😊");
    push(
        &mut checks,
        "表情不参与重排",
        emoji_at.is_some_and(|index| !promoted.promote(index)),
        format!("表情下标 {emoji_at:?}"),
    );

    // 单字候选后不插表情,数字选同音字的序号保持不变。
    let mut single = mini();
    letters(&mut single, "si");
    let single_list = single.candidates();
    push(
        &mut checks,
        "单字不插表情",
        top(&single) == "四" && !single_list.iter().any(|text| text == "4️⃣"),
        format!("{single_list:?}"),
    );

    // 英文:非拼音串给出英文词;大小写按词表;半截拼音不出现原文候选。
    let mut english = mini();
    letters(&mut english, "hello");
    let english_hint = english
        .candidates_detailed()
        .first()
        .and_then(|cand| cand.hint.as_deref());
    push(
        &mut checks,
        "英文混输候选",
        top(&english) == "hello" && english_hint == Some("英文"),
        format!("{:?} 提示 {english_hint:?}", english.candidates()),
    );
    let events = english.process(Key::Space);
    let exported = english.export_user();
    push(
        &mut checks,
        "英文上屏不学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "hello"))
            && exported.is_empty(),
        format!("{events:?}"),
    );

    let mut casing = mini();
    letters(&mut casing, "iphone");
    push(
        &mut checks,
        "英文专名大小写",
        top(&casing) == "iPhone",
        format!("{:?}", casing.candidates()),
    );

    // v 是快捷输入前缀,但只有命中符号/数字/日期等触发串时才让位;
    // `vscode` 这类 v 开头英文词仍应正常混输。
    let mut vword = mini();
    letters(&mut vword, "vscode");
    push(
        &mut checks,
        "v 开头英文词",
        top(&vword) == "VSCode",
        format!("{:?}", vword.candidates()),
    );

    let mut prefix = mini();
    letters(&mut prefix, "nih");
    push(
        &mut checks,
        "英文不打断拼音前缀",
        !prefix.candidates().iter().any(|text| text == "nih") && top(&prefix) != "nih",
        format!("{:?}", prefix.candidates()),
    );

    // 简繁:歧义短语、单字换形与繁体一字多形。
    let cases = [
        ("toufa", "頭髮"),
        ("ganjing", "乾淨"),
        ("ganbei", "乾杯"),
        ("huanghou", "皇后"),
        ("houlai", "後來"),
        ("limian", "裏面"),
        ("zhizao", "製造"),
        ("yizhi", "一隻"),
        ("taiwan", "臺灣"),
        ("yunduan", "雲端"),
        ("choulou", "醜陋"),
        ("fazhan", "發展"),
        ("houtai", "後臺"),
        ("miantiao", "麪條"),
    ];
    for (raw, want) in cases {
        let mut engine = custom(true);
        letters(&mut engine, raw);
        let actual = top(&engine);
        push(
            &mut checks,
            "简繁转换",
            actual == want && display(&engine) == want,
            format!("{raw} → {actual}"),
        );
    }

    // 繁体显示、简体学习:词库保持简体,切回简体不召回繁体词。
    let mut canonical = custom(true);
    letters(&mut canonical, "toufa");
    let events = canonical.process(Key::Space);
    let exported = canonical.export_user();
    push(
        &mut checks,
        "繁体上屏学简体",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "頭髮"))
            && exported
                .lines()
                .any(|line| line.starts_with("tou'fa\t头发\t"))
            && !exported.contains("頭髮"),
        format!("词库 {}", exported.lines().count()),
    );

    canonical.set_config(Config::default());
    letters(&mut canonical, "toufa");
    push(
        &mut checks,
        "切回简体不召回繁体",
        !canonical.candidates().iter().any(|text| text == "頭髮") && top(&canonical) == "头发",
        format!("{:?}", canonical.candidates()),
    );

    // 组合中切换:候选与内联显示同时换字形。
    let mut switch = custom(false);
    letters(&mut switch, "toufa");
    let simplified = top(&switch) == "头发";
    switch.set_config(Config {
        traditional: true,
        ..Config::default()
    });
    let traditional = top(&switch) == "頭髮" && display(&switch) == "頭髮";
    switch.set_config(Config::default());
    push(
        &mut checks,
        "组合中切换简繁",
        simplified && traditional && top(&switch) == "头发" && display(&switch) == "头发",
        format!("当前 {:?}", switch.candidates()),
    );

    // 繁体模式不碰 ASCII:英文候选与回车原样上屏都保持原样。
    let mut ascii = custom(true);
    letters(&mut ascii, "hello");
    let ascii_top = top(&ascii);
    let events = ascii.process(Key::Space);
    push(
        &mut checks,
        "繁体不影响英文",
        ascii_top == "hello"
            && events
                .iter()
                .any(|event| matches!(event, Event::Commit(text) if text == "hello")),
        format!("{ascii_top:?} {events:?}"),
    );

    let mut raw = custom(true);
    letters(&mut raw, "toufa");
    let events = raw.process(Key::Enter);
    push(
        &mut checks,
        "繁体不影响原文上屏",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "toufa")),
        format!("{events:?}"),
    );

    report.push_str("\n## 核心输入补齐\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0;
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

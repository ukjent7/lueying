//! 快捷输入 E2E:日期、时间、星期、v 模式数字/金额/日期/算式与 u 问字。
//!
//! 先列失败方式:
//! 1. 宿主没注入本地时间 → 日期/时间用 UTC,跨时区或午夜会错一天。
//! 2. 触发串不精确 → `rqx` 也出日期,或者长句里的 `rq` 被截出来。
//! 3. 快捷候选写进用户词库 → 第二天同一触发串返回昨天学到的旧日期。
//! 4. 快捷候选参与词级转移 → 日期/金额成为下一条候选的前词。
//! 5. v 模式数字仍当候选序号 → 根本打不出 `v123`。
//! 6. 小数点被当标点落定 → 打不出金额小数。
//! 7. 中文数字零/万/亿与开头「一十」读错。
//! 8. 金额角分/整/零的规则错。
//! 9. 快捷候选被停顿重排换走 → 空格上屏另一个词(Server 侧检查)。
//! 10. 回删、回车退路与组合残留坏 → 快捷输入吞键或留下半个数字。
//! 11. 快捷项抢掉可信整词的首选 → `sj` 应保留「时间」,不能直接跳到时间候选。
//! 12. 快捷文本写进字级搭配 → 数字/日期会污染后续候选的个人上下文。
//! 13. 失焦落定或 Shift+Delete 把快捷项当用户词 → 学出搭配或误删同文词。
//! 14. 全拼打出「日期/时间/星期」时不跟写法,或写法抢掉词本身的首选。
//! 15. 显式触发与关键词触发重复插入同一组写法。
//! 16. v 表达式把运算符当翻页/标点 → 打不出 `v1+2`。
//! 17. 算式不校验语法、除零与溢出 → 上屏 Infinity、空串或半个式子。
//! 18. 四位年份日期不校验月日/闰年 → 把非法日期当日期,或漏掉点分隔写法。
//! 19. u 问字把数字当候选序号,或纯字母被误判成码点 → 打不出 `u4e00` 或抢掉拼音。
//! 20. 码点不校验代理区、控制字符与上限 → 上屏乱码或控制字符。

use std::sync::{Arc, RwLock};

use lueying_dict::Dictionary;
use lueying_engine::{CivilTime, Config, Engine, Event, Key, SharedUser, UserDict};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

fn engine() -> Engine {
    Engine::new(Arc::new(Dictionary::builtin()), Config::default())
}

fn at(now: CivilTime) -> Engine {
    let mut e = engine();
    e.set_now(now);
    e
}

/// 预先学一个词,模拟全拼命中「日期/时间/星期」时的用户词库。
fn with_word(word: &str, key: &str, now: CivilTime) -> Engine {
    let user: SharedUser = Arc::new(RwLock::new(UserDict::new()));
    user.write()
        .unwrap_or_else(|p| p.into_inner())
        .learn(key, word);
    let mut e = Engine::new(Arc::new(Dictionary::builtin()), Config::default()).with_user(user);
    e.set_now(now);
    e
}

fn fixed() -> CivilTime {
    CivilTime {
        year: 2026,
        month: 10,
        day: 26,
        hour: 14,
        minute: 5,
        second: 9,
        weekday: 1,
    }
}

fn letters(e: &mut Engine, text: &str) {
    for c in text.chars() {
        e.process(Key::Letter(c));
    }
}

/// 先按 v,再把数字、小数点与运算符打进组合;`x` 当乘号。
fn expression(e: &mut Engine, body: &str) {
    e.process(Key::Letter('v'));
    for c in body.chars() {
        if let Some(d) = c.to_digit(10) {
            e.process(Key::Digit(d as u8));
        } else if c == 'x' {
            e.process(Key::Letter('x'));
        } else {
            e.process(Key::Punct(c));
        }
    }
}

fn numbers(e: &mut Engine, body: &str) {
    expression(e, body);
}

fn cands(body: &str) -> Vec<String> {
    let mut e = engine();
    numbers(&mut e, body);
    e.candidates()
}

fn expr_cands(body: &str) -> Vec<String> {
    let mut e = engine();
    expression(&mut e, body);
    e.candidates()
}

/// 先按 u,再按可选的 `+` 与十六进制码点。
fn unicode(e: &mut Engine, body: &str) {
    e.process(Key::Letter('u'));
    for c in body.chars() {
        if let Some(d) = c.to_digit(10) {
            e.process(Key::Digit(d as u8));
        } else if c == '+' {
            e.process(Key::Punct('+'));
        } else {
            e.process(Key::Letter(c));
        }
    }
}

fn unicode_cands(body: &str) -> Vec<String> {
    let mut e = engine();
    unicode(&mut e, body);
    e.candidates()
}

fn top_is(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    body: &str,
    want: &str,
) {
    let top = cands(body).into_iter().next().unwrap_or_default();
    push(checks, name, top == want, format!("v{body} → {top}"));
}

fn has(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    body: &str,
    want: &str,
) {
    let list = cands(body);
    push(
        checks,
        name,
        list.iter().any(|c| c == want),
        format!("v{body} → {list:?}"),
    );
}

fn expr_top(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    body: &str,
    want: &str,
) {
    let top = expr_cands(body).into_iter().next().unwrap_or_default();
    push(checks, name, top == want, format!("v{body} → {top}"));
}

fn expr_has(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    body: &str,
    want: &str,
) {
    let list = expr_cands(body);
    push(
        checks,
        name,
        list.iter().any(|c| c == want),
        format!("v{body} → {list:?}"),
    );
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 日期:固定本地时间,四种格式;数字选词仍然可用,且不写用户词库。
    let mut d = at(fixed());
    letters(&mut d, "rq");
    let list = d.candidates();
    push(
        &mut checks,
        "日期触发",
        list.first().map(String::as_str) == Some("2026年10月26日"),
        format!("{list:?}"),
    );
    push(
        &mut checks,
        "日期多格式",
        ["2026-10-26", "2026/10/26", "10月26日"]
            .iter()
            .all(|want| list.iter().any(|c| c == want)),
        format!("{list:?}"),
    );
    let events = d.process(Key::Digit(2));
    push(
        &mut checks,
        "日期数字选词",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "2026-10-26")),
        format!("{events:?}"),
    );
    let exported = d.export_user();
    push(
        &mut checks,
        "日期不写词库",
        !exported.contains("2026年10月26日") && !exported.lines().any(|line| line.starts_with("rq\t")),
        exported.lines().next().unwrap_or_default().to_string(),
    );
    // 时间前进一天后同一触发必须给出新日期,不能召回上次上屏的旧字串。
    d.set_now(CivilTime {
        day: 27,
        weekday: 2,
        ..fixed()
    });
    letters(&mut d, "rq");
    push(
        &mut checks,
        "日期不记旧值",
        d.candidates().first().map(String::as_str) == Some("2026年10月27日"),
        format!("{:?}", d.candidates()),
    );

    // 时间:有「时间」这个拼音首选时快捷项排在它后面,不抢词库首选。
    let mut t = at(fixed());
    letters(&mut t, "sj");
    let list = t.candidates();
    let time_at = list.iter().position(|c| c == "14:05");
    push(
        &mut checks,
        "时间触发",
        time_at.is_some() && time_at != Some(0),
        format!("{list:?}"),
    );
    push(
        &mut checks,
        "时间多格式",
        ["14:05:09", "14点05分", "下午2:05", "20261026140509"]
            .iter()
            .all(|want| list.iter().any(|c| c == want)),
        format!("{list:?}"),
    );
    let mut midnight = at(CivilTime {
        hour: 0,
        minute: 5,
        second: 0,
        ..fixed()
    });
    letters(&mut midnight, "sj");
    push(
        &mut checks,
        "凌晨时间",
        midnight.candidates().iter().any(|c| c == "00:05")
            && midnight.candidates().iter().any(|c| c == "凌晨0:05"),
        format!("{:?}", midnight.candidates()),
    );
    let mut noon = at(CivilTime {
        hour: 12,
        minute: 0,
        ..fixed()
    });
    letters(&mut noon, "sj");
    push(
        &mut checks,
        "中午时间",
        noon.candidates().iter().any(|c| c == "中午12:00"),
        format!("{:?}", noon.candidates()),
    );

    // 星期:固定星期一,同时给「星期一」「周一」。
    let mut w = at(fixed());
    letters(&mut w, "xq");
    push(
        &mut checks,
        "星期触发",
        w.candidates().first().map(String::as_str) == Some("星期一")
            && w.candidates().iter().any(|c| c == "周一"),
        format!("{:?}", w.candidates()),
    );

    // 全拼打出词本身时,对应写法紧跟在这个词后面,不抢它的首选。
    let mut kd = with_word("日期", "ri'qi", fixed());
    letters(&mut kd, "riqi");
    let list = kd.candidates();
    push(
        &mut checks,
        "全拼日期关键词",
        list.first().map(String::as_str) == Some("日期")
            && list.get(1).map(String::as_str) == Some("2026年10月26日"),
        format!("{list:?}"),
    );

    let mut kt = at(fixed());
    letters(&mut kt, "shijian");
    let list = kt.candidates();
    push(
        &mut checks,
        "全拼时间关键词",
        list.first().map(String::as_str) == Some("时间")
            && list.get(1).map(String::as_str) == Some("14:05"),
        format!("{list:?}"),
    );

    let mut kx = with_word("星期", "xing'qi", fixed());
    letters(&mut kx, "xingqi");
    let list = kx.candidates();
    push(
        &mut checks,
        "全拼星期关键词",
        list.first().map(String::as_str) == Some("星期")
            && list.get(1).map(String::as_str) == Some("星期一"),
        format!("{list:?}"),
    );

    // 关键词候选同样不学习;词库在选快捷项前后必须一模一样。
    let before = kd.export_user();
    let events = kd.process(Key::Digit(2));
    let after = kd.export_user();
    push(
        &mut checks,
        "关键词不重复学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "2026年10月26日"))
            && before == after,
        format!("{events:?}"),
    );

    // 关键词与显式触发只出一份,不能把同一日期插两遍;可信用户词仍保留首选。
    let mut dup = with_word("日期", "ri'qi", fixed());
    letters(&mut dup, "rq");
    let list = dup.candidates();
    let dates = list.iter().filter(|text| text.as_str() == "2026年10月26日").count();
    push(
        &mut checks,
        "显式触发不重复",
        list.first().map(String::as_str) == Some("日期") && dates == 1,
        format!("{list:?}"),
    );

    // 触发必须精确到整个组合;多一个字母就不再出日期。
    let mut exact = at(fixed());
    letters(&mut exact, "rqx");
    push(
        &mut checks,
        "触发串精确",
        !exact.candidates().iter().any(|c| c == "2026年10月26日"),
        format!("{:?}", exact.candidates()),
    );

    // v 模式:数字进入组合,空格上屏;金额与逐位读法同时提供。
    let mut v = engine();
    numbers(&mut v, "123");
    push(
        &mut checks,
        "v 模式数字入组合",
        v.composition().map(|comp| comp.raw.as_str()) == Some("v123"),
        format!("{:?}", v.composition().map(|comp| comp.raw.clone())),
    );
    let list = v.candidates();
    push(
        &mut checks,
        "v 整数读法",
        list.first().map(String::as_str) == Some("一百二十三"),
        format!("{list:?}"),
    );
    push(
        &mut checks,
        "v 金额与逐位",
        list.iter().any(|c| c == "壹佰贰拾叁")
            && list.iter().any(|c| c == "壹佰贰拾叁元整")
            && list.iter().any(|c| c == "一二三"),
        format!("{list:?}"),
    );
    // 数字键继续补数字,而不是选第二条候选。
    v.process(Key::Digit(2));
    push(
        &mut checks,
        "v 数字不选词",
        v.composition().map(|comp| comp.raw.as_str()) == Some("v1232")
            && v.candidates().first().map(String::as_str) == Some("一千二百三十二"),
        format!("{:?}", v.composition().map(|comp| comp.raw.clone())),
    );
    let events = v.process(Key::Space);
    push(
        &mut checks,
        "v 空格上屏",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "一千二百三十二")),
        format!("{events:?}"),
    );
    let exported = v.export_user();
    push(
        &mut checks,
        "v 结果不写词库",
        !exported.lines().any(|line| {
            line.starts_with("@context\t")
                || (!line.starts_with('@') && line.contains("一千二百三十二"))
        }),
        exported.lines().next().unwrap_or_default().to_string(),
    );

    // 金额:小数两位、角、分、零、整的规则;小写金额与大写数字都要有。
    has(&mut checks, "金额角分", "100.06", "壹佰元零陆分");
    has(&mut checks, "金额到角", "100.50", "壹佰元伍角");
    has(&mut checks, "金额到分", "100.56", "壹佰元伍角陆分");
    has(&mut checks, "金额整", "100.00", "壹佰元整");
    has(&mut checks, "金额零元", "0.05", "伍分");
    has(&mut checks, "金额小写", "123.5", "一百二十三元五角");
    has(&mut checks, "金额小写十", "10.5", "十元五角");

    // 位值读法:零、万、亿、跨亿组合与前导「一十」。
    top_is(&mut checks, "十几读法", "12", "十二");
    top_is(&mut checks, "中间零", "1001", "一千零一");
    top_is(&mut checks, "千位零", "1010", "一千零一十");
    top_is(&mut checks, "十万零一", "100001", "十万零一");
    top_is(&mut checks, "两节连读", "20003000", "二千万三千");
    top_is(&mut checks, "节间补零", "20000300", "二千万零三百");
    top_is(&mut checks, "亿零一", "100000001", "一亿零一");
    top_is(&mut checks, "亿零一万", "100010000", "一亿零一万");
    top_is(&mut checks, "亿节连读", "120000300", "一亿二千万零三百");
    top_is(
        &mut checks,
        "跨亿组合",
        "1234567890123",
        "一万二千三百四十五亿六千七百八十九万零一百二十三",
    );
    top_is(&mut checks, "万亿", "1000000000000", "一万亿");
    has(&mut checks, "大额金额", "100200.3", "壹拾万零贰佰元叁角");

    // v 表达式:运算符进组合、按优先级求值,结果不学习。
    let mut calc = engine();
    expression(&mut calc, "1+2");
    push(
        &mut checks,
        "表达式进组合",
        calc.composition().map(|comp| comp.raw.as_str()) == Some("v1+2")
            && calc.candidates().first().map(String::as_str) == Some("3"),
        format!("{:?}", calc.candidates()),
    );
    let events = calc.process(Key::Space);
    let exported = calc.export_user();
    push(
        &mut checks,
        "表达式上屏不学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "3"))
            && exported.is_empty(),
        exported.lines().next().unwrap_or_default().to_string(),
    );
    expr_top(&mut checks, "表达式优先级", "1+2*3", "7");
    expr_top(&mut checks, "表达式括号", "(1+2)*3", "9");
    expr_top(&mut checks, "表达式乘号 x", "2x3+4", "10");
    expr_top(&mut checks, "表达式乘方", "2^3^2", "512");
    expr_top(&mut checks, "表达式负号", "-3+5", "2");
    expr_top(&mut checks, "表达式小数", "1.5*2", "3");
    expr_top(&mut checks, "表达式除法", "1/3", "0.3333333333");
    expr_has(&mut checks, "表达式带原式", "1+2", "1+2=3");
    push(
        &mut checks,
        "表达式除零无候选",
        expr_cands("1/0").is_empty(),
        format!("{:?}", expr_cands("1/0")),
    );
    push(
        &mut checks,
        "表达式语法错无候选",
        expr_cands("1+").is_empty() && expr_cands("(1+2").is_empty(),
        format!("{:?}", expr_cands("1+")),
    );

    // v 日期:四位年份 + 月日才认,非法日期回落到算式。
    let mut date = engine();
    expression(&mut date, "1998-3-8");
    let list = date.candidates();
    push(
        &mut checks,
        "日期表达式进组合",
        date.composition().map(|comp| comp.raw.as_str()) == Some("v1998-3-8")
            && list.first().map(String::as_str) == Some("一九九八年三月八日"),
        format!("{list:?}"),
    );
    push(
        &mut checks,
        "日期表达式多格式",
        ["1998年3月8日", "1998-03-08"]
            .iter()
            .all(|want| list.iter().any(|c| c == want)),
        format!("{list:?}"),
    );
    expr_top(&mut checks, "日期表达式斜杠", "1998/3/8", "一九九八年三月八日");
    expr_top(
        &mut checks,
        "日期表达式点分隔",
        "2006.12.25",
        "二〇〇六年十二月二十五日",
    );
    expr_top(&mut checks, "非法日期回落算式", "1998-2-30", "1966");
    expr_top(&mut checks, "非四位年份当算式", "98-3-8", "87");
    expr_top(&mut checks, "日期段数不对当算式", "1998-3-8-1", "1986");

    // u 问字:十六进制码点直接出字符,数字不能当候选序号。
    let mut uni = engine();
    unicode(&mut uni, "4e00");
    push(
        &mut checks,
        "u 问字入组合",
        uni.composition().map(|comp| comp.raw.as_str()) == Some("u4e00")
            && uni.candidates().first().map(String::as_str) == Some("一"),
        format!("{:?}", uni.candidates()),
    );
    let events = uni.process(Key::Space);
    let exported = uni.export_user();
    push(
        &mut checks,
        "u 问字上屏不学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "一"))
            && exported.is_empty(),
        exported.lines().next().unwrap_or_default().to_string(),
    );
    push(
        &mut checks,
        "u 问字加号显式",
        unicode_cands("+1f600").first().map(String::as_str) == Some("😀"),
        format!("{:?}", unicode_cands("+1f600")),
    );
    push(
        &mut checks,
        "u 问字基本平面",
        unicode_cands("41").first().map(String::as_str) == Some("A"),
        format!("{:?}", unicode_cands("41")),
    );
    push(
        &mut checks,
        "u 问字纯字母不加号",
        !unicode_cands("fade")
            .iter()
            .any(|text| text == "\u{fade}"),
        format!("{:?}", unicode_cands("fade")),
    );
    push(
        &mut checks,
        "u 问字拒绝控制符",
        unicode_cands("07").is_empty() && unicode_cands("d800").is_empty(),
        format!("{:?}", unicode_cands("07")),
    );
    push(
        &mut checks,
        "u 问字拒绝超范围",
        unicode_cands("110000").is_empty(),
        format!("{:?}", unicode_cands("110000")),
    );

    // 日期上屏同样不写个人搭配:整串都是汉字,漏标 quick 会立刻学出字对。
    let mut date_flow = engine();
    expression(&mut date_flow, "1998-3-8");
    date_flow.process(Key::Space);
    let exported = date_flow.export_user();
    push(
        &mut checks,
        "日期表达式不学习",
        exported.is_empty(),
        exported.lines().next().unwrap_or_default().to_string(),
    );

    // 末尾刚敲的点仍按整数出候选,数字进来后再切成小数,候选不闪。
    let mut dot = engine();
    numbers(&mut dot, "1.");
    push(
        &mut checks,
        "v 末尾点按整数",
        dot.composition().map(|comp| comp.raw.as_str()) == Some("v1.")
            && dot.candidates().first().map(String::as_str) == Some("一"),
        format!("{:?}", dot.candidates()),
    );
    dot.process(Key::Digit(2));
    push(
        &mut checks,
        "v 小数读法",
        dot.candidates().first().map(String::as_str) == Some("一点二"),
        format!("{:?}", dot.candidates()),
    );

    let mut raw = engine();
    numbers(&mut raw, "123");
    let events = raw.process(Key::Enter);
    push(
        &mut checks,
        "v 回车原样上屏",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "v123")),
        format!("{events:?}"),
    );

    let mut back = engine();
    numbers(&mut back, "123");
    back.process(Key::Backspace);
    push(
        &mut checks,
        "v 回删后重算",
        back.composition().map(|comp| comp.raw.as_str()) == Some("v12")
            && back.candidates().first().map(String::as_str) == Some("十二"),
        format!("{:?}", back.candidates()),
    );

    // 日期上屏后紧接着打拼音,不能把日期写成下一条候选的前词。
    let mut flow = at(fixed());
    letters(&mut flow, "rq");
    flow.process(Key::Space);
    letters(&mut flow, "xian");
    flow.process(Key::Space);
    let exported = flow.export_user();
    push(
        &mut checks,
        "日期不污染词序",
        !exported
            .lines()
            .any(|line| line.starts_with("@transition\t2026年")),
        exported.lines().next().unwrap_or_default().to_string(),
    );

    // 失焦落定也按快捷语义:上屏星期,但「星期一」不写个人搭配。
    let mut fin = at(fixed());
    letters(&mut fin, "xq");
    let text = fin.finalize_display();
    let exported = fin.export_user();
    push(
        &mut checks,
        "失焦落定快捷",
        text.as_deref() == Some("星期一"),
        format!("{text:?}"),
    );
    push(
        &mut checks,
        "失焦不学搭配",
        !exported
            .lines()
            .any(|line| line.starts_with("@context\t星\t期")),
        exported.lines().next().unwrap_or_default().to_string(),
    );

    // 快捷项没有学习记录,Shift+Delete 不能按同文删掉真正的词。
    let mut forget = at(fixed());
    letters(&mut forget, "rq");
    push(
        &mut checks,
        "快捷不可忘",
        !forget.forget_highlighted(),
        format!("{:?}", forget.candidates()),
    );

    report.push_str("\n## 快捷输入\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0;
    for (name, ok, note) in &checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if *ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}

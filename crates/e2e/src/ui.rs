//! 候选窗鼠标交互的引擎契约。先列失败方式,再逐条检查:
//!
//! 1. 翻页越界仍改页/高亮 → 边界必须停住。
//! 2. 点选越界误上屏或丢组合 → 必须原样不动。
//! 3. 点选上屏的不是所点候选 → 必须与当前页序号一致,并只学它吃掉的音节。
//! 4. 键盘翻页与鼠标翻页状态不一致 → 两者走同一套 page/highlight。
//! 5. `-`/`=` 在单页或边界吞掉标点 → 只在多页时翻页,否则按标点上屏。
//! 6. 无中文映射的标点直通 → 先收组句再按 ASCII 上屏,引擎与文档不能错位。
//! 7. 组合中按到空候选位的数字 → 先收组句再上屏数字,不能留下内联拼音。
//! 8. 组合中按 Shift+字母 → 整串落定再上屏大写,不能把大写插进拼音。
//! 9. 失焦/别的窗口切模式时只落定前缀 → 剩余音节必须继续按首选上屏。
//! 10. 部分选词后光标跑到剩余拼音前面 → 继续输入必须接在末尾。
//! 11. 长句超过组合上限后被截断或吞键 → 常见长句要完整保留,到上限时先落定
//!     旧组合再用新键起组。
//! 12. 换输入框后沿用上个文档的引号奇偶 → 新文档首个引号必须是开引号。
//! 13. 正确整句被分段前缀挤出首页 → 首页五条优先整句,前缀词顺延到第 6 位。

use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Event, Key, MAX_RAW};

fn engine() -> Engine {
    Engine::new(Arc::new(Dictionary::builtin()), Config::default())
}

fn type_into(engine: &mut Engine, raw: &str) {
    for c in raw.chars() {
        engine.process(Key::Letter(c));
    }
}

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

/// 用户词库导出里是否还有某个词(行格式:`键<TAB>词<TAB>频次<TAB>时间戳`)。
fn learned(export: &str, word: &str) -> bool {
    export
        .lines()
        .filter(|line| !line.starts_with('@'))
        .any(|line| line.split('\t').nth(1) == Some(word))
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 键盘翻页:PageDown/PageUp 与鼠标 `page_by` 共用状态。
    let mut e = engine();
    type_into(&mut e, "yi");
    e.process(Key::PageDown);
    let down = (e.page(), e.highlight());
    e.process(Key::PageUp);
    let up = (e.page(), e.highlight());
    push(
        &mut checks,
        "键盘翻页移动页与高亮",
        down == (1, e.page_size()) && up == (0, 0),
        format!("down={down:?} up={up:?}"),
    );

    // 方向键跨过页边界:高亮进入下一页时页号必须一起走,
    // 否则候选窗按当前页取项,高亮项根本不在窗口里。
    let mut e = engine();
    type_into(&mut e, "yi");
    let ps = e.page_size();
    if e.candidates().len() > ps {
        for _ in 0..ps.saturating_sub(1) {
            e.process(Key::Down);
        }
        let before = (e.page(), e.highlight());
        e.process(Key::Down);
        let down = (e.page(), e.highlight());
        e.process(Key::Up);
        let up = (e.page(), e.highlight());
        push(
            &mut checks,
            "方向键跨页时页号跟随",
            before == (0, ps - 1) && down == (1, ps) && up == (0, ps - 1),
            format!("{before:?} → {down:?} → {up:?}"),
        );
    } else {
        push(
            &mut checks,
            "方向键跨页时页号跟随",
            false,
            "内置词库 yi 候选不足一页".into(),
        );
    }

    // 翻页/移动高亮后继续打字:候选已经换了一批,页号和高亮必须回到第一条,
    // 否则旧页号越界时候选窗会空着,页码还会显示成 3/1。
    let mut e = engine();
    type_into(&mut e, "shi");
    let ps = e.page_size();
    if e.candidates().len() > ps {
        let _ = e.page_by(1);
        e.process(Key::Down);
        let before = (e.page(), e.highlight());
        e.process(Key::Letter('j'));
        let after = (e.page(), e.highlight());
        push(
            &mut checks,
            "继续打字后候选回到第一条",
            before.0 > 0 && before.1 > 0 && after == (0, 0),
            format!("{before:?} → {after:?}"),
        );
    } else {
        push(
            &mut checks,
            "继续打字后候选回到第一条",
            false,
            "内置词库 shi 候选不足一页".into(),
        );
    }

    // 高亮换候选时内联文本必须一起变;否则空格上屏的内容与眼前看到的不一致。
    let mut e = engine();
    type_into(&mut e, "shi");
    let second = e.candidates().get(1).cloned().unwrap_or_default();
    e.process(Key::Down);
    let display = e
        .composition()
        .map(|comp| comp.display.clone())
        .unwrap_or_default();
    push(
        &mut checks,
        "高亮候选同步内联文本",
        !second.is_empty() && display == second,
        format!("内联 {display:?} / 第二候选 {second:?}"),
    );

    // Shift+Delete 忘掉误学的候选:学习记录必须从用户词库消失;
    // 空组合里该键要直通给应用。
    let mut e = engine();
    type_into(&mut e, "shi");
    let second = e.candidates().get(1).cloned().unwrap_or_default();
    e.process(Key::Digit(2));
    let saved = e.export_user();
    type_into(&mut e, "shi");
    // 选过一次就回到首位,与微软拼音、微信一致。
    let promoted = e.candidates().first() == Some(&second);
    // 学过的词可能已经被排序到首位,也可能仍在原位;先定位再高亮。
    let index = e.candidates().iter().position(|c| c == &second);
    if let Some(index) = index {
        e.set_highlight(index);
    }
    e.process(Key::DeleteCandidate);
    let after = e.export_user();
    // 删除后仍保持原候选高亮,再按一次不会误删别的学习词。
    let kept = e.candidates().get(e.highlight()) == Some(&second);
    let repeat = e.process(Key::DeleteCandidate);
    e.process(Key::Esc);
    let idle = e.process(Key::DeleteCandidate);
    push(
        &mut checks,
        "Shift+Delete 删除学习记录",
        !second.is_empty()
            && learned(&saved, &second)
            && promoted
            && index.is_some()
            && !learned(&after, &second)
            && kept
            && repeat.is_empty()
            && idle
                .iter()
                .any(|event| matches!(event, Event::Passthrough(Key::DeleteCandidate))),
        format!("学过 {second:?} → 删除后剩余 {}", after.lines().count()),
    );

    // 搜索框的 FinalizeExactCompositionString:落定当前显示文本,不补全剩余拼音。
    let mut e = engine();
    type_into(&mut e, "nihao");
    let shown = e.candidates().first().cloned().unwrap_or_default();
    let finalized = e.finalize_display();
    let idle = e.finalize_display();
    push(
        &mut checks,
        "宿主原样落定当前显示文本",
        !shown.is_empty()
            && finalized.as_deref() == Some(shown.as_str())
            && e.composition().is_none()
            && e.candidates().is_empty()
            && idle.is_none(),
        format!("显示 {shown:?} → 落定 {finalized:?}"),
    );

    // 长句候选:默认一页五条全部给整句,最高分前缀词顺延到第 6 位;
    // 正确读法不会被人名/单字的分段入口挤出首页,逐段上屏下移一格即可。
    let mut e = engine();
    type_into(&mut e, "jintiantianqi");
    let detailed = e.candidates_detailed();
    let available = detailed.iter().filter(|c| c.full).count();
    let first_page = detailed.iter().take(5).filter(|c| c.full).count();
    let prefix_sixth = detailed.get(5).is_some_and(|c| !c.full);
    push(
        &mut checks,
        "整句备选占满候选首页",
        available >= 5 && first_page == 5 && prefix_sixth,
        format!("整句 {available} 条,首页 {first_page} 条,第六位前缀词={prefix_sixth}"),
    );

    // 分段上屏学到的前缀词不能顶掉整句首选:选过一次「今天」后重打整句,
    // 首位仍应是整句候选,「今天」留在前缀词位置。
    let mut e = engine();
    type_into(&mut e, "jintiantianqi");
    let slot = e.candidates().iter().position(|c| c == "今天");
    if let Some(index) = slot {
        // 前缀词现在在第 6 位;先移动高亮再空格上屏,不依赖当前页的序号。
        e.set_highlight(index);
        e.process(Key::Space);
    }
    e.process(Key::Esc);
    type_into(&mut e, "jintiantianqi");
    let detailed = e.candidates_detailed();
    let top_is_sentence = detailed.first().is_some_and(|c| c.full && c.text != "今天");
    let prefix_kept = detailed.iter().any(|c| c.text == "今天" && !c.full);
    push(
        &mut checks,
        "学过的前缀词不顶掉整句",
        slot.is_some() && top_is_sentence && prefix_kept,
        format!(
            "首位 {:?},仍保留前缀={prefix_kept}",
            detailed.first().map(|c| (c.text.as_str(), c.full))
        ),
    );

    // 宿主自绘候选时按全局序号移动高亮(TSF 的 SetSelection):
    // 页号必须跟着高亮走,GetCurrentPage 才能把候选画到正确的页。
    let mut e = engine();
    type_into(&mut e, "yi");
    let ps = e.page_size();
    let count = e.candidates().len();
    let pages = count.div_ceil(ps);
    let moved = e.set_highlight(ps);
    let second = (e.page(), e.highlight());
    e.set_highlight(count.saturating_sub(1));
    let last = (e.page(), e.highlight());
    let home = e.set_highlight(0);
    push(
        &mut checks,
        "宿主移动高亮时页号跟随",
        pages > 1
            && moved
            && second == (1, ps)
            && last == (pages - 1, count - 1)
            && home
            && (e.page(), e.highlight()) == (0, 0),
        format!("pages={pages} second={second:?} last={last:?}"),
    );

    // 翻页越界:停在上/下边界。
    let mut e = engine();
    type_into(&mut e, "yi");
    let last = e
        .candidates()
        .len()
        .div_ceil(e.page_size())
        .saturating_sub(1);
    while e.page() < last {
        let _ = e.page_by(1);
    }
    let beyond = e.page_by(1);
    let at_end = (e.page(), e.highlight());
    push(
        &mut checks,
        "翻页越界停住",
        !beyond && at_end == (last, last * e.page_size()),
        format!("last={last} at={at_end:?}"),
    );

    // 点选越界:不上屏、不丢组合。
    let mut e = engine();
    type_into(&mut e, "yi");
    let before = e.candidates();
    let events = e.pick_in_page(999);
    push(
        &mut checks,
        "点选越界不动组合",
        events.is_empty() && e.composition().is_some() && e.candidates() == before,
        format!("{} 条候选", before.len()),
    );

    // 点第二页第一条:上屏的必须正是该条,学习键是完整输入。
    let mut e = engine();
    type_into(&mut e, "yi");
    let page_size = e.page_size();
    let cands = e.candidates();
    let _ = e.page_by(1);
    let expected = cands.get(page_size).cloned().unwrap_or_default();
    let events = e.pick_in_page(0);
    let committed: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "点选上屏所点候选",
        !expected.is_empty()
            && committed == vec![expected.clone()]
            && e.composition().is_none()
            && e.export_user().contains(&format!("yi\t{expected}\t")),
        format!("上屏 {committed:?}"),
    );

    // `-`/`=` 多页翻页;首页再按 `-` 停在边界,不把候选连同标点一起上屏。
    let mut e = engine();
    type_into(&mut e, "yi");
    let pages = e.candidates().len().div_ceil(e.page_size());
    if pages > 1 {
        let minus = e.process(Key::Punct('-'));
        let stayed = e.page() == 0 && minus.is_empty();
        e.process(Key::Punct('='));
        let moved = e.page() == 1;
        push(
            &mut checks,
            "-/= 多页翻页且边界停住",
            stayed && moved && e.composition().is_some(),
            format!("pages={pages} page={}", e.page()),
        );
    } else {
        push(
            &mut checks,
            "-/= 多页翻页且边界停住",
            false,
            "内置词库 yi 只有一页,无法验证".into(),
        );
    }

    // 无中文映射的 `/`:先把首选上屏,再上屏 ASCII 斜杠。
    let mut e = engine();
    type_into(&mut e, "ni");
    let best = e.candidates().first().cloned().unwrap_or_default();
    let events = e.process(Key::Punct('/'));
    let committed: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "无映射标点先收组句再上屏 ASCII",
        committed == vec![best.clone(), "/".into()] && e.composition().is_none(),
        format!("上屏 {committed:?}"),
    );

    // 换输入框后引号重新从开引号配对;沿用上一个文档的奇偶会打出闭引号。
    let mut e = engine();
    let first = e.process(Key::Punct('"'));
    e.clear_context();
    let second = e.process(Key::Punct('"'));
    push(
        &mut checks,
        "失焦后引号重新配对",
        first == vec![Event::Commit("“".into())]
            && second == vec![Event::Commit("“".into())],
        format!("先 {first:?},换框后 {second:?}"),
    );

    // 关闭中文标点后,逗号同样按 ASCII 上屏并结束组句。
    let mut e = Engine::new(
        Arc::new(Dictionary::builtin()),
        Config {
            chinese_punct: false,
            ..Config::default()
        },
    );
    type_into(&mut e, "ni");
    let best = e.candidates().first().cloned().unwrap_or_default();
    let events = e.process(Key::Punct(','));
    let committed: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "关闭中文标点仍先收组句",
        committed == vec![best, ",".into()] && e.composition().is_none(),
        format!("上屏 {committed:?}"),
    );

    // 当前页没有这个序号:组合必须整串落定后再上屏数字。
    let mut e = engine();
    type_into(&mut e, "ni");
    let best = e.candidates().first().cloned().unwrap_or_default();
    let committed: Vec<String> = e
        .process(Key::Digit(0))
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "空候选位数字先收组句",
        committed == vec![best, "0".into()] && e.composition().is_none(),
        format!("上屏 {committed:?}"),
    );

    // Shift+字母(协议里是大写 Letter):同样先落定整串,再上屏大写。
    let mut e = engine();
    type_into(&mut e, "ni");
    let best = e.candidates().first().cloned().unwrap_or_default();
    let committed: Vec<String> = e
        .process(Key::Letter('A'))
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "组合中大写字母先收组句",
        committed == vec![best, "A".into()] && e.composition().is_none(),
        format!("上屏 {committed:?}"),
    );

    // 部分选词后还有剩余音节:失焦或全局切模式必须整串落定,
    // 只按一次空格会把剩余拼音永远留在组合里。
    let mut e = engine();
    type_into(&mut e, "jintiantianqi");
    let slot = e.candidates().iter().position(|c| c == "今天");
    let picked: Vec<String> = slot
        .and_then(|index| u8::try_from(index + 1).ok())
        .map(|digit| e.process(Key::Digit(digit)))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    let flushed: Vec<String> = e
        .flush()
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "整串落定剩余音节",
        slot.is_some()
            && picked == vec!["今天".to_string()]
            && flushed == vec!["天气".to_string()]
            && e.composition().is_none(),
        format!("候选位 {slot:?},先 {picked:?},再 {flushed:?}"),
    );

    // 部分选词后光标必须留在剩余拼音末尾:下一个字母接在后面,
    // 不能插到最前面把 `tianqi` 变成 `htianqi`。
    let mut e = engine();
    type_into(&mut e, "jintiantianqi");
    let slot = e.candidates().iter().position(|c| c == "今天");
    if let Some(digit) = slot.and_then(|index| u8::try_from(index + 1).ok()) {
        e.process(Key::Digit(digit));
    }
    let before = e.composition().map(|comp| comp.raw.clone());
    type_into(&mut e, "h");
    let after = e.composition().map(|comp| comp.raw.clone());
    push(
        &mut checks,
        "部分选词后继续输入接在末尾",
        slot.is_some() && before.as_deref() == Some("tianqi") && after.as_deref() == Some("tianqih"),
        format!("候选位 {slot:?},选后 {before:?},再输入 h 后 {after:?}"),
    );

    // 长句不能被 40 字母的旧上限截断:45 个字母的常见句子要完整留在组合里,
    // 否则末尾拼音直接丢失,正确整句候选永远出不来。
    let mut e = engine();
    let long = "gaodewohaihuaiyishibushizijidacuoguanjiancile";
    type_into(&mut e, long);
    let kept = e.composition().map(|comp| comp.raw.clone()).unwrap_or_default();
    push(
        &mut checks,
        "长句完整保留在组合中",
        kept == long,
        format!("输入 {} 字母,组合保留 {} 字母", long.len(), kept.len()),
    );

    // 到达上限后继续按键:先落定旧组合,再用新键起组,不能把键吞掉。
    let mut e = engine();
    let full: String = "a".repeat(MAX_RAW);
    type_into(&mut e, &full);
    let at_limit = e.composition().map(|comp| comp.raw.len()).unwrap_or(0);
    let events = e.process(Key::Letter('a'));
    let committed = events.iter().any(|event| matches!(event, Event::Commit(_)));
    let restart = e
        .composition()
        .map(|comp| (comp.raw.as_str(), comp.cursor))
        .unwrap_or_default();
    push(
        &mut checks,
        "组合上限先落定再起新组",
        at_limit == MAX_RAW
            && committed
            && restart == ("a", 1)
            && !events.iter().any(|event| matches!(event, Event::Passthrough(_))),
        format!("上限 {at_limit},上屏 {committed},新组合 {restart:?}"),
    );

    report.push_str("\n## 候选窗交互\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
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

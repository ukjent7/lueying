//! 设置页的纯文本:引擎状态行与数字读法。

use lueying_protocol::EngineStatus;

/// 「关于」里的状态行:词典来源与规模、模型是否加载。
pub fn status_detail(status: &Option<EngineStatus>) -> String {
    match status {
        Some(s) => format!(
            "词典 {}({}) · 语法模型{} · 重排 4M{} / 25M{}",
            word_count(s.dict_words),
            s.dict_source,
            mark(s.gram),
            mark(s.rerank4m),
            mark(s.rerank25m),
        ),
        None => "服务未运行,按任意键后再看".to_string(),
    }
}

/// 词条数的人类读法:上万用「万」,否则原样。
fn word_count(words: usize) -> String {
    if words >= 10_000 {
        let w = words as f32 / 10_000.0;
        if (w * 10.0).round() as i32 % 10 == 0 {
            format!("{w:.0} 万")
        } else {
            format!("{w:.1} 万")
        }
    } else {
        format!("{words} 词条")
    }
}

fn mark(on: bool) -> &'static str {
    if on { "✓" } else { "✗" }
}

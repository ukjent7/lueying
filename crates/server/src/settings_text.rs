//! 设置页的纯文本:引擎状态行与数字读法。

use lueying_protocol::{EngineStatus, RerankLatency};

/// 「关于」里的状态行:词典来源与规模、模型是否加载、本机重排延迟。
pub fn status_detail(status: &Option<EngineStatus>) -> String {
    match status {
        Some(s) => {
            let mut detail = format!(
                "词典 {}({}) · 语法模型{} · 重排 4M{} / 25M{}",
                word_count(s.dict_words),
                s.dict_source,
                mark(s.gram),
                mark(s.rerank4m),
                mark(s.rerank25m),
            );
            for (label, latency) in [("4M", &s.rerank_sync), ("25M", &s.rerank_settle)] {
                let line = latency_detail(label, latency);
                if !line.is_empty() {
                    detail.push_str(" · ");
                    detail.push_str(&line);
                }
            }
            detail
        }
        None => "服务未运行,按任意键后再看".to_string(),
    }
}

/// 只在真的跑过模型时显示延迟,刚启动的空直方图不占版面。
fn latency_detail(label: &str, latency: &RerankLatency) -> String {
    if latency.count == 0 {
        return String::new();
    }
    format!(
        "{label} p50 {:.1}ms / p95 {:.1}ms({} 次)",
        latency.p50_us as f32 / 1000.0,
        latency.p95_us as f32 / 1000.0,
        latency.count
    )
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

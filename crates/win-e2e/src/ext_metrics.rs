//! 扩展运行指标:服务端写下的快照里,shot-ocr 要有状态、内存与派发计数。

use std::time::Duration;

/// 等快照出现,再检查 shot-ocr 一行。
pub fn check(data: &std::path::Path) -> Result<String, String> {
    let path = data.join("lueying").join("logs").join("ext-metrics.json");
    for _ in 0..30 {
        if path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !path.exists() {
        return Err(format!("扩展指标没有写入: {}", path.display()));
    }
    let metrics = lueying_extmeta::metrics::ExtMetrics::load(&path);
    let Some(stat) = metrics.extensions.iter().find(|stat| stat.id == "shot-ocr") else {
        return Err(format!("扩展指标里没有 shot-ocr: {metrics:?}"));
    };
    if stat.state != lueying_extmeta::metrics::ExtState::Loaded {
        return Err(format!(
            "shot-ocr 状态是 {:?},不是运行: {metrics:?}",
            stat.state
        ));
    }
    if stat.memory_used == 0 || stat.memory_limit == 0 || stat.dispatches == 0 {
        return Err(format!("shot-ocr 缺少内存或派发指标: {stat:?}"));
    }
    Ok(format!(
        "## 扩展运行指标\n\n`{}`\n\n{}",
        path.display(),
        metrics.summary()
    ))
}

//! 扩展运行指标:服务端写下的快照里,shot-ocr 要有状态、内存与派发计数。

use std::time::{Duration, Instant};

/// 等快照出现,再等 shot-ocr 的行就绪;快照每秒才落一次盘,
/// 文件存在不代表内容已经反映最近的加载与派发。
pub fn check(data: &std::path::Path) -> Result<String, String> {
    let path = data.join("lueying").join("logs").join("ext-metrics.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last: Option<String> = None;
    loop {
        if path.exists() {
            let metrics = lueying_extmeta::metrics::ExtMetrics::load(&path);
            match metrics.extensions.iter().find(|stat| stat.id == "shot-ocr") {
                Some(stat)
                    if stat.state == lueying_extmeta::metrics::ExtState::Loaded
                        && stat.memory_used > 0
                        && stat.memory_limit > 0
                        && stat.dispatches > 0 =>
                {
                    return Ok(format!(
                        "## 扩展运行指标\n\n`{}`\n\n{}",
                        path.display(),
                        metrics.summary()
                    ));
                }
                Some(_) => last = Some(format!("{metrics:?}")),
                None => return Err(format!("扩展指标里没有 shot-ocr: {metrics:?}")),
            }
        }
        if Instant::now() >= deadline {
            return Err(match last {
                Some(debug) => format!("shot-ocr 指标 15 秒内没有就绪: {debug}"),
                None => format!("扩展指标没有写入: {}", path.display()),
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

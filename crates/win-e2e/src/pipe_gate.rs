//! 管道授权门的 E2E 断言:E2E 进程不是本产品工具,Manage 请求必须被拒,
//! 只读请求与打字流照常放行。对端是真实常驻服务,授权链路全真。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::fs::OpenOptionsExt;

use lueying_protocol::{Reply, Request};
use windows::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

/// 以本进程身份发一次请求-应答。与 TSF 同款 SQOS:禁止对端模拟自己。
fn round_trip(request: &Request) -> Result<Reply, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(SECURITY_SQOS_PRESENT.0 | SECURITY_IDENTIFICATION.0)
        .open(lueying_protocol::pipe_name())
        .map_err(|_| "常驻服务管道连不上".to_string())?;
    let mut writer = file;
    let mut reader = BufReader::new(file.try_clone().map_err(|e| e.to_string())?);
    let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|e| format!("写管道: {e}"))?;
    let mut reply = String::new();
    reader
        .read_line(&mut reply)
        .map_err(|e| format!("读应答: {e}"))?;
    serde_json::from_str(reply.trim()).map_err(|e| format!("应答不是合法 Reply: {e}"))
}

/// 非工具进程的特权请求要被拒成中性应答,同用户的只读请求要放行。
pub(super) fn check() -> Result<String, String> {
    let denied = round_trip(&Request::ExportUser)?;
    if !denied.user.is_empty() {
        return Err("非工具进程拿到了用户词库导出,授权门失效".into());
    }
    let status = round_trip(&Request::Status)?;
    if status.status.is_none() {
        return Err("同用户只读请求被错误拒绝,应答没有资源状态".into());
    }
    Ok(
        "## 管道授权门\n\n非工具进程的用户词库导出被拒;同用户只读请求放行,打字流不受影响(本文件其余断言即证据)。"
            .into(),
    )
}

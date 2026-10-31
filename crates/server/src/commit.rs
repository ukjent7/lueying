//! 对外 commit 通道:鉴权、用户确认与隐私门禁,平台无关。
//!
//! 任务书 6.3:其他本地工具可借用输入法的上屏能力,无需自己做输入法;
//! 必须有调用方鉴权、管道 ACL、用户确认,且永不向隐私字段注入。
//! - 用户确认:配置键 `commit_channel`,默认关闭,设置里显式打开;
//! - 调用方鉴权:64 位十六进制随机令牌,存 `%APPDATA%\lueying\commit-token`,
//!   只在通道开启后生成,调用方随请求携带;
//! - 管道 ACL:沿用主管道的 SDDL(本用户与应用包),远程客户端被拒;
//! - 隐私字段:活动会话标记 private 时一律拒绝。
//!
//! 通道默认关;开着的通道只多一条请求分派路径,按键热路径不经过它。

use std::path::{Path, PathBuf};

/// 令牌字节数;十六进制后 64 字符。
pub const TOKEN_BYTES: usize = 32;

/// 三道门,按「用户确认 → 鉴权 → 隐私」的顺序判;`Err` 是给人看的原因。
///
/// 鉴权放在隐私检查之前:令牌错的请求不值得暴露会话状态。
pub fn decide(
    enabled: bool,
    expected: Option<&str>,
    presented: &str,
    private: bool,
) -> Result<(), String> {
    if !enabled {
        return Err("对外 commit 通道未开启;在设置里打开 commit_channel".to_string());
    }
    let Some(expected) = expected else {
        return Err("鉴权令牌尚未生成;确认通道开启后重试".to_string());
    };
    // 本地管道上的时序侧信道没有现实收益,按字节相等比较。
    if presented.as_bytes() != expected.trim().as_bytes() {
        return Err("鉴权令牌不匹配".to_string());
    }
    if private {
        return Err("隐私字段下不注入文本".to_string());
    }
    Ok(())
}

/// 令牌文件路径;`base` 是用户配置目录(Windows 为 `%APPDATA%\lueying`)。
pub fn token_path(base: &Path) -> PathBuf {
    base.join("commit-token")
}

/// 读令牌;没有就生成一个并落盘。文件权限沿用用户目录默认 ACL,
/// 其他本机用户读不到。失败方向是调用失败,不静默降级成无鉴权。
pub fn ensure_token(path: &Path) -> Result<String, String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let text = text.trim();
        if !text.is_empty() {
            return Ok(text.to_string());
        }
    }
    let token = generate_token()?;
    std::fs::write(path, &token).map_err(|error| format!("写令牌失败: {error}"))?;
    Ok(token)
}

/// 32 字节 CSPRNG → 64 位十六进制。 randomness 用 ed25519-dalek 复用的
/// rand_core OsRng,不为此引入新依赖。
pub fn generate_token() -> Result<String, String> {
    use ed25519_dalek::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| format!("生成令牌失败: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
#[path = "commit_tests.rs"]
mod tests;

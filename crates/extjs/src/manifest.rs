//! 扩展包清单:字段校验与入口脚本名约束。
//!
//! 清单是扩展与输入法之间唯一的静态契约。加载期把能拒绝的都拒绝:
//! 未知字段、过高的场景契约、任何能力声明、越界的入口路径。运行期不再为
//! 坏清单兜底,也不做「猜作者意图」的兼容。

use serde::Deserialize;

use crate::SCENE_API;

/// 清单文件的大小上限。清单只放元数据,超过这个数说明放错了东西。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// 入口脚本的大小上限。打包后的单文件 IIFE 通常几十 KB,留足余量。
pub const MAX_SOURCE_BYTES: usize = 512 * 1024;

/// 扩展清单,对应包内的 `manifest.json`。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// 扩展 id,同时是包目录名。
    pub id: String,
    /// 显示名;设置页与日志用。
    pub name: String,
    /// 扩展自己的版本号,只用于显示。
    pub version: String,
    /// 依赖的场景协议版本;高于 [`SCENE_API`] 拒绝加载。
    pub api: u32,
    /// 入口脚本,包内文件名;默认 `index.js`。
    #[serde(default = "default_main")]
    pub main: String,
    /// 声明需要的能力。当前版本不授予任何能力,非空即拒绝。
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_main() -> String {
    "index.js".to_string()
}

impl Manifest {
    /// 解析并校验清单。
    ///
    /// 未知字段、非 UTF-8、类型不符都返回可读原因;调用方直接把原因写进
    /// 日志与设置页,不需要再解释。
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("清单解析失败: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// 校验字段取值;目录名与 id 的一致性由 [`crate::Pack`] 检查。
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!(
                "扩展 id {:?} 不合法:只允许小写字母、数字、- 与 _,且以字母或数字开头",
                self.id
            ));
        }
        let name_len = self.name.chars().count();
        if self.name.trim().is_empty() {
            return Err("显示名为空".into());
        }
        if name_len > 80 {
            return Err(format!("显示名长度 {name_len} 超出 1..=80"));
        }
        if self.name.chars().any(char::is_control) {
            return Err("显示名含控制字符".into());
        }
        if !valid_version(&self.version) {
            return Err(format!("版本号 {:?} 不合法", self.version));
        }
        if self.api == 0 || self.api > SCENE_API {
            return Err(format!(
                "扩展需要场景协议 {},当前只支持 1..={SCENE_API}",
                self.api
            ));
        }
        if !valid_main(&self.main) {
            return Err(format!("入口脚本 main {:?} 不合法", self.main));
        }
        if !self.capabilities.is_empty() {
            return Err(format!(
                "扩展声明了能力 {:?},当前版本不授予任何能力",
                self.capabilities.join(", ")
            ));
        }
        Ok(())
    }
}

/// id 只允许小写字母、数字、`-`、`_`,以字母或数字开头,最长 48 字节。
fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    id.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 版本号只做字符集约束,不强制语义化版本。
fn valid_version(version: &str) -> bool {
    let mut chars = version.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    version.len() <= 32
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+')
}

/// 入口脚本是包内裸文件名:不能带路径分隔符,不能是 `..`,必须以 `.js` 结尾。
fn valid_main(name: &str) -> bool {
    name.len() > 3
        && name.len() <= 64
        && name.ends_with(".js")
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;

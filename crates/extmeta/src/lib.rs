//! 扩展包清单:一个包可以同时带外观、逻辑与能力组件。
//!
//! 清单是扩展与输入法之间唯一的静态契约。加载期把能拒绝的都拒绝:
//! 未知字段、越界的组件路径、未知权限、过高的 API 版本。运行期不再
//! 为坏清单兜底,也不猜作者意图。
//!
//! 旧版外观包(`{ "api": 1, "main": "index.js" }`)继续可用;新清单把
//! 组件分开写,每个组件有自己的 API 版本与权限。

use serde::Deserialize;

/// 清单文件的大小上限。清单只放元数据,超过这个数说明放错了东西。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// 外观组件当前 API。
pub const APPEARANCE_API: u32 = 1;
/// 逻辑组件当前 API。
pub const LOGIC_API: u32 = 1;
/// 能力组件当前 API。
pub const PANEL_API: u32 = 1;

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
    /// 旧版外观包:顶层 API 版本。
    #[serde(default)]
    pub api: Option<u32>,
    /// 旧版外观包:顶层入口脚本,默认 `index.js`。
    #[serde(default)]
    pub main: Option<String>,
    /// 旧版能力声明;必须为空。
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 候选窗外观组件。
    #[serde(default)]
    pub appearance: Option<Appearance>,
    /// 逻辑组件(WASM)。
    #[serde(default)]
    pub logic: Option<Logic>,
    /// 进程外能力组件。
    #[serde(default)]
    pub panel: Option<Panel>,
}

/// 候选窗外观组件:入口是一段 JS。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    /// 场景协议版本。
    pub api: u32,
    /// 入口脚本,包内裸文件名。
    #[serde(default = "default_main")]
    pub main: String,
}

/// 逻辑组件:入口是一个 WASM 模块。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Logic {
    /// 逻辑 ABI 版本。
    pub api: u32,
    /// 模块文件,包内裸文件名。
    pub module: String,
    /// 申请的权限。
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

/// 能力组件:入口是一个独立可执行文件。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    /// 面板协议版本。
    pub api: u32,
    /// 可执行文件名,包内裸文件名。
    pub command: String,
    /// 申请的权限。
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

/// 扩展能申请的权限。按组件授权,默认一个都不给。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// 读取候选文字,用于过滤与重排。
    CandidateText,
    /// 截取屏幕区域。
    ScreenCapture,
    /// 对截取内容做文字识别。
    Ocr,
    /// 访问网络。
    Network,
    /// 读写剪贴板。
    Clipboard,
}

impl Permission {
    /// 全部权限;设置页与诊断按这个顺序列。
    pub const ALL: [Self; 5] = [
        Self::CandidateText,
        Self::ScreenCapture,
        Self::Ocr,
        Self::Network,
        Self::Clipboard,
    ];

    /// 清单里的权限名。
    pub fn key(self) -> &'static str {
        match self {
            Self::CandidateText => "candidate-text",
            Self::ScreenCapture => "screen-capture",
            Self::Ocr => "ocr",
            Self::Network => "network",
            Self::Clipboard => "clipboard",
        }
    }

    /// 这个权限是否属于该组件。
    pub fn allowed_for(self, component: Component) -> bool {
        match self {
            Self::CandidateText => component == Component::Logic,
            Self::ScreenCapture | Self::Ocr | Self::Network | Self::Clipboard => {
                component == Component::Panel
            }
        }
    }
}

/// 清单里的组件类型;错误信息与权限校验用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    Appearance,
    Logic,
    Panel,
}

impl Component {
    /// 中文名。
    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "外观",
            Self::Logic => "逻辑",
            Self::Panel => "能力",
        }
    }
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
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "清单超过 {MAX_MANIFEST_BYTES} 字节(实际 {})",
                bytes.len()
            ));
        }
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("清单解析失败: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// 校验字段取值;目录名与 id 的一致性由加载方检查。
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!(
                "扩展 id {:?} 不合法:只允许小写字母、数字、- 与 _,且以字母或数字开头",
                self.id
            ));
        }
        check_name(&self.name)?;
        if !valid_version(&self.version) {
            return Err(format!("版本号 {:?} 不合法", self.version));
        }
        if !self.capabilities.is_empty() {
            return Err(format!(
                "扩展声明了旧版能力 {:?},请改用组件权限",
                self.capabilities.join(", ")
            ));
        }

        let legacy = self.api.is_some() || self.main.is_some();
        if legacy && (self.appearance.is_some() || self.logic.is_some() || self.panel.is_some()) {
            return Err("旧版顶层 api/main 不能和 appearance/logic/panel 混用".into());
        }
        let mut components = 0;
        if legacy {
            let api = self.api.ok_or("旧版清单缺少 api")?;
            check_api(Component::Appearance, api, APPEARANCE_API)?;
            check_entry("入口脚本", self.main.as_deref().unwrap_or("index.js"), "js")?;
            components += 1;
        }
        if let Some(appearance) = &self.appearance {
            check_api(Component::Appearance, appearance.api, APPEARANCE_API)?;
            check_entry("入口脚本", &appearance.main, "js")?;
            components += 1;
        }
        if let Some(logic) = &self.logic {
            check_api(Component::Logic, logic.api, LOGIC_API)?;
            check_entry("逻辑模块", &logic.module, "wasm")?;
            check_permissions(Component::Logic, &logic.permissions)?;
            components += 1;
        }
        if let Some(panel) = &self.panel {
            check_api(Component::Panel, panel.api, PANEL_API)?;
            check_entry("能力入口", &panel.command, "exe")?;
            check_permissions(Component::Panel, &panel.permissions)?;
            components += 1;
        }
        if components == 0 {
            return Err("清单没有可加载的组件".into());
        }
        Ok(())
    }

    /// 外观入口:新清单的 `appearance.main`,或旧版顶层 `main`。
    pub fn appearance_main(&self) -> Option<&str> {
        if let Some(appearance) = &self.appearance {
            return Some(&appearance.main);
        }
        self.api.map(|_| self.main.as_deref().unwrap_or("index.js"))
    }

    /// 外观 API 版本:新清单的 `appearance.api`,或旧版顶层 `api`。
    pub fn appearance_api(&self) -> Option<u32> {
        self.appearance.as_ref().map(|a| a.api).or(self.api)
    }

    /// 是否只有外观组件。设置页的「运行期脚本」只列这类包。
    pub fn is_appearance_only(&self) -> bool {
        self.appearance_main().is_some() && self.logic.is_none() && self.panel.is_none()
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

/// 显示名 1–80 字,不能含控制字符。
fn check_name(name: &str) -> Result<(), String> {
    let len = name.chars().count();
    if name.trim().is_empty() {
        return Err("显示名为空".into());
    }
    if len > 80 {
        return Err(format!("显示名长度 {len} 超出 1..=80"));
    }
    if name.chars().any(char::is_control) {
        return Err("显示名含控制字符".into());
    }
    Ok(())
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

/// 组件入口是包内裸文件名:不能带路径分隔符,不能是 `..`,后缀必须匹配。
fn check_entry(label: &str, name: &str, extension: &str) -> Result<(), String> {
    let suffix = format!(".{extension}");
    if name.len() <= suffix.len()
        || name.len() > 64
        || !name.ends_with(&suffix)
        || name.starts_with('.')
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!("{label} {name:?} 不合法"));
    }
    Ok(())
}

/// API 版本必须在 1..=当前版本。
fn check_api(component: Component, api: u32, current: u32) -> Result<(), String> {
    if api == 0 || api > current {
        return Err(format!(
            "{}组件需要 API {api},当前支持 1..={current}",
            component.title()
        ));
    }
    Ok(())
}

/// 权限必须属于该组件,且不能重复声明。
fn check_permissions(component: Component, permissions: &[Permission]) -> Result<(), String> {
    let mut seen = Vec::new();
    for permission in permissions {
        if !permission.allowed_for(component) {
            return Err(format!(
                "{}组件不能申请 {}",
                component.title(),
                permission.key()
            ));
        }
        if seen.contains(permission) {
            return Err(format!("权限 {} 重复声明", permission.key()));
        }
        seen.push(*permission);
    }
    Ok(())
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;

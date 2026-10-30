//! 扩展包清单:一个包可以同时带外观、逻辑与能力组件。
//!
//! 清单是扩展与输入法之间唯一的静态契约。加载期把能拒绝的都拒绝:
//! 未知字段、越界的组件路径、未知权限、过高的 API 版本。运行期不再
//! 为坏清单兜底,也不猜作者意图。
//!
//! 旧版外观包(`{ "api": 1, "main": "index.js" }`)继续可用。API 2 起
//! 清单直接描述脚本层:入口、激活方式(触发词/热键/命令)、需要的宿主
//! 服务与能力。命令只声明,不拥有按键;绑定由用户决定。

use serde::Deserialize;

pub mod consent;
pub mod metrics;
mod sha256;

/// 清单文件的大小上限。清单只放元数据,超过这个数说明放错了东西。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// 外观组件当前 API。
pub const APPEARANCE_API: u32 = 1;
/// 逻辑组件当前 API。
pub const LOGIC_API: u32 = 1;
/// 能力组件当前 API。
pub const PANEL_API: u32 = 1;
/// 脚本层清单当前 API。
pub const SCRIPT_API: u32 = 2;

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
    /// 清单 API 版本:1 是旧版外观包,2 是脚本层清单。
    #[serde(default)]
    pub api: Option<u32>,
    /// 脚本入口,包内裸文件名;API 2 默认 `index.js`。
    #[serde(default)]
    pub main: Option<String>,
    /// API 2 的激活方式。
    #[serde(default)]
    pub activate: Option<Activate>,
    /// API 2 需要的宿主服务。
    #[serde(default)]
    pub needs: Vec<Service>,
    /// 申请的权限;API 1 必须为空。
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 候选窗外观组件(API 1)。
    #[serde(default)]
    pub appearance: Option<Appearance>,
    /// 逻辑组件(WASM,API 1)。
    #[serde(default)]
    pub logic: Option<Logic>,
    /// 进程外能力组件(API 1)。
    #[serde(default)]
    pub panel: Option<Panel>,
}

/// API 2 的激活声明。三种入口都只是声明,按键绑定仍归用户。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Activate {
    /// 输入内触发词;整串完全匹配才出现动作候选。
    #[serde(default)]
    pub triggers: Vec<String>,
    /// 建议热键;宿主只把它当建议,实际绑定在用户表里。
    #[serde(default)]
    pub hotkeys: Vec<String>,
    /// 扩展提供的命令。
    #[serde(default)]
    pub commands: Vec<Command>,
}

/// 一条命令声明。命令 id 在扩展内唯一,完整 id 是 `<扩展 id>.<命令 id>`。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    /// 命令 id,扩展内唯一。
    pub id: String,
    /// 命令面板与动作候选里显示的文字。
    pub title: String,
    /// 绑定生效的输入状态;默认任何场景。
    #[serde(default)]
    pub when: When,
}

/// 命令绑定生效的输入状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum When {
    /// 任何场景。
    #[default]
    Any,
    /// 当前焦点可编辑。
    Editable,
    /// 正在组句。
    Composing,
}

/// 宿主服务。扩展声明需要什么,宿主按需提供;没声明就调用失败。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Service {
    /// 截取屏幕区域或窗口。
    Capture,
    /// 对图像做文字识别。
    Ocr,
    /// 读写剪贴板。
    Clipboard,
    /// 宿主绘制的声明式面板。
    Panel,
    /// 把结果送到当前输入框。
    Output,
    /// 注册全局热键。
    Hotkey,
    /// 访问网络(域名白名单)。
    Net,
    /// 扩展私有存储目录。
    Store,
    /// 系统通知。
    Notify,
}

impl Service {
    /// 全部服务;设置页与诊断按这个顺序列。
    pub const ALL: [Self; 9] = [
        Self::Capture,
        Self::Ocr,
        Self::Clipboard,
        Self::Panel,
        Self::Output,
        Self::Hotkey,
        Self::Net,
        Self::Store,
        Self::Notify,
    ];

    /// 清单里的服务名。
    pub fn key(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Ocr => "ocr",
            Self::Clipboard => "clipboard",
            Self::Panel => "panel",
            Self::Output => "output",
            Self::Hotkey => "hotkey",
            Self::Net => "net",
            Self::Store => "store",
            Self::Notify => "notify",
        }
    }

    /// 授权页上的人话;与能力名重复的由 [`Service::permission`] 合并。
    pub fn title(self) -> &'static str {
        match self {
            Self::Capture => "截取屏幕(高敏感)",
            Self::Ocr => "识别文字",
            Self::Clipboard => "读写剪贴板(高敏感)",
            Self::Panel => "显示扩展面板",
            Self::Output => "写入当前输入框",
            Self::Hotkey => "注册全局热键",
            Self::Net => "访问网络(外发)",
            Self::Store => "读写扩展存储",
            Self::Notify => "系统通知",
        }
    }

    /// 这个服务对应的权限;没有对应权限的服务单独列进授权说明。
    pub fn permission(self) -> Option<Permission> {
        match self {
            Self::Capture => Some(Permission::ScreenCapture),
            Self::Ocr => Some(Permission::Ocr),
            Self::Clipboard => Some(Permission::Clipboard),
            Self::Net => Some(Permission::Network),
            Self::Panel | Self::Output | Self::Hotkey | Self::Store | Self::Notify => None,
        }
    }
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

    /// 授权页上的人话与敏感级别。读拼音原文是低敏感,读左邻/屏幕内容是高敏感。
    pub fn title(self) -> &'static str {
        match self {
            Self::CandidateText => "读取候选与上下文文字(高敏感)",
            Self::ScreenCapture => "截取屏幕(高敏感)",
            Self::Ocr => "识别文字",
            Self::Network => "访问网络(外发)",
            Self::Clipboard => "读写剪贴板(高敏感)",
        }
    }

    /// 会读到用户内容、需要和「外发」组合时强提示。
    pub fn reads_user_content(self) -> bool {
        matches!(
            self,
            Self::CandidateText | Self::ScreenCapture | Self::Ocr | Self::Clipboard
        )
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

        let components = self.appearance.is_some() || self.logic.is_some() || self.panel.is_some();
        match self.api {
            Some(SCRIPT_API) => {
                if components {
                    return Err("API 2 清单不能和 appearance/logic/panel 混用".into());
                }
                check_entry("入口脚本", self.main.as_deref().unwrap_or("index.js"), "js")?;
                if !self.capabilities.is_empty() {
                    check_capabilities(&self.capabilities)?;
                }
                check_services(&self.needs)?;
                if let Some(activate) = &self.activate {
                    activate.validate()?;
                }
                Ok(())
            }
            Some(1) => {
                if components {
                    return Err("旧版顶层 api/main 不能和 appearance/logic/panel 混用".into());
                }
                check_entry("入口脚本", self.main.as_deref().unwrap_or("index.js"), "js")?;
                if self.activate.is_some() || !self.needs.is_empty() {
                    return Err("旧版清单不能声明 activate/needs".into());
                }
                if !self.capabilities.is_empty() {
                    return Err(format!(
                        "扩展声明了旧版能力 {:?},请改用组件权限",
                        self.capabilities.join(", ")
                    ));
                }
                Ok(())
            }
            Some(api) => Err(format!("清单 API {api} 不支持,当前支持 1..={SCRIPT_API}")),
            None => {
                if !self.capabilities.is_empty() {
                    return Err(format!(
                        "扩展声明了旧版能力 {:?},请改用组件权限",
                        self.capabilities.join(", ")
                    ));
                }
                if self.activate.is_some() || !self.needs.is_empty() || self.main.is_some() {
                    return Err("没有 api 的清单不能声明 main/activate/needs".into());
                }
                let mut count = 0;
                if let Some(appearance) = &self.appearance {
                    check_api(Component::Appearance, appearance.api, APPEARANCE_API)?;
                    check_entry("入口脚本", &appearance.main, "js")?;
                    count += 1;
                }
                if let Some(logic) = &self.logic {
                    check_api(Component::Logic, logic.api, LOGIC_API)?;
                    check_entry("逻辑模块", &logic.module, "wasm")?;
                    check_permissions(Component::Logic, &logic.permissions)?;
                    count += 1;
                }
                if let Some(panel) = &self.panel {
                    check_api(Component::Panel, panel.api, PANEL_API)?;
                    check_entry("能力入口", &panel.command, "exe")?;
                    check_permissions(Component::Panel, &panel.permissions)?;
                    count += 1;
                }
                if count == 0 {
                    return Err("清单没有可加载的组件".into());
                }
                Ok(())
            }
        }
    }

    /// 脚本入口:API 2 的 `main`,或外观组件的入口,或旧版顶层 `main`。
    pub fn script_main(&self) -> Option<&str> {
        match self.api {
            Some(SCRIPT_API) => Some(self.main.as_deref().unwrap_or("index.js")),
            Some(1) => Some(self.main.as_deref().unwrap_or("index.js")),
            _ => self
                .appearance
                .as_ref()
                .map(|appearance| appearance.main.as_str()),
        }
    }

    /// 旧名保留:外观入口与脚本入口是同一个文件。
    pub fn appearance_main(&self) -> Option<&str> {
        self.script_main()
    }

    /// 外观 API 版本:新清单的 `appearance.api`,或旧版顶层 `api`。
    pub fn appearance_api(&self) -> Option<u32> {
        self.appearance.as_ref().map(|a| a.api).or(self.api)
    }

    /// 是否只有外观组件。设置页的「运行期脚本」列所有带脚本入口的包。
    pub fn is_appearance_only(&self) -> bool {
        self.script_main().is_some() && self.logic.is_none() && self.panel.is_none()
    }

    /// 激活声明;旧版包没有。
    pub fn activate(&self) -> Option<&Activate> {
        self.activate.as_ref()
    }

    /// 命令声明;旧版包返回空表。
    pub fn commands(&self) -> &[Command] {
        self.activate
            .as_ref()
            .map(|activate| activate.commands.as_slice())
            .unwrap_or(&[])
    }
}

impl Activate {
    /// 校验触发词、热键与命令声明。
    pub fn validate(&self) -> Result<(), String> {
        if self.triggers.is_empty() && self.hotkeys.is_empty() && self.commands.is_empty() {
            return Err("activate 至少要有 triggers、hotkeys 或 commands 之一".into());
        }
        let mut triggers = Vec::new();
        for trigger in &self.triggers {
            check_trigger(trigger)?;
            if triggers.contains(trigger) {
                return Err(format!("触发词 {trigger:?} 重复声明"));
            }
            triggers.push(trigger.clone());
        }
        for hotkey in &self.hotkeys {
            if hotkey.is_empty() || hotkey.len() > 64 || hotkey.chars().any(char::is_control) {
                return Err(format!("热键 {hotkey:?} 不合法"));
            }
        }
        let mut commands = Vec::new();
        for command in &self.commands {
            check_command(command)?;
            if commands.contains(&command.id) {
                return Err(format!("命令 {} 重复声明", command.id));
            }
            commands.push(command.id.clone());
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

/// 命令 id 与扩展 id 同一套字符规则。
fn check_command(command: &Command) -> Result<(), String> {
    if !valid_id(&command.id) {
        return Err(format!(
            "命令 id {:?} 不合法:只允许小写字母、数字、- 与 _,且以字母或数字开头",
            command.id
        ));
    }
    if command.title.trim().is_empty() || command.title.chars().count() > 80 {
        return Err(format!("命令 {} 的标题为空或超过 80 字", command.id));
    }
    if command.title.chars().any(char::is_control) {
        return Err(format!("命令 {} 的标题含控制字符", command.id));
    }
    Ok(())
}

/// 触发词是输入内整串匹配的文本:不能为空、不能含空白或控制字符。
fn check_trigger(trigger: &str) -> Result<(), String> {
    let len = trigger.chars().count();
    if len == 0 || len > 32 {
        return Err(format!("触发词 {trigger:?} 长度必须在 1..=32"));
    }
    if trigger.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("触发词 {trigger:?} 不能含空白或控制字符"));
    }
    Ok(())
}

/// API 2 的能力名必须在已知权限里,且不能重复。
fn check_capabilities(capabilities: &[String]) -> Result<(), String> {
    let mut seen = Vec::new();
    for capability in capabilities {
        let permission = Permission::ALL
            .into_iter()
            .find(|permission| permission.key() == capability)
            .ok_or_else(|| format!("未知能力 {capability:?}"))?;
        if seen.contains(&permission) {
            return Err(format!("能力 {capability:?} 重复声明"));
        }
        seen.push(permission);
    }
    Ok(())
}

/// 宿主服务不能重复声明。
fn check_services(services: &[Service]) -> Result<(), String> {
    let mut seen = Vec::new();
    for service in services {
        if seen.contains(service) {
            return Err(format!("宿主服务 {} 重复声明", service.key()));
        }
        seen.push(*service);
    }
    Ok(())
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

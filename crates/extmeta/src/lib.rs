//! 扩展包清单:一个包就是一份清单加一个 Wasm 模块。
//!
//! 清单是扩展与输入法之间唯一的静态契约。加载期把能拒绝的都拒绝:
//! 未知字段、越界的模块路径、未知权限、过高的 API 版本。运行期不再
//! 为坏清单兜底,也不猜作者意图。
//!
//! API 4 起扩展只有一种形态:Wasm 组件(`wasm-tools component new`
//! 打包出的 `.wasm`),由宿主用 wasmtime 在进程内执行。外观、命令与设置
//! 都从同一份清单声明;不再有 JS 入口、组件式外观或独立能力进程。

use serde::Deserialize;

pub mod consent;
pub mod container;
pub mod metrics;
pub mod sha256;

/// 清单文件的大小上限。清单只放元数据,超过这个数说明放错了东西。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// 当前清单 API。
pub const EXT_API: u32 = 4;

/// 文本类设置的最大长度;密钥太长没有实际用途,也避免撑爆输入框。
pub const MAX_TEXT_CHARS: usize = 4096;

/// 扩展形态。Wasm 在宿主进程内解释执行;原生是独立进程的服务提供者,
/// 由扩展宿主按需拉起并沙箱化。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Wasm 模块(默认)。
    #[default]
    Wasm,
    /// 原生可执行文件。
    Native,
}

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
    /// 清单 API 版本;当前只接受 [`EXT_API`]。
    pub api: u32,
    /// 扩展形态;缺省是 Wasm。
    #[serde(default)]
    pub kind: Kind,
    /// Wasm 模块,包内裸文件名;仅 [`Kind::Wasm`]。
    #[serde(default)]
    pub wasm: String,
    /// 原生可执行文件,包内裸文件名;仅 [`Kind::Native`]。
    #[serde(default)]
    pub exe: String,
    /// 是否提供候选窗外观;为 true 时模块必须导出 `lueying_appearance`。
    #[serde(default)]
    pub appearance: bool,
    /// 内嵌语言引擎(JS 引擎、CPython 等重运行时)的组件声明此项,
    /// 宿主按引擎级资源预算载入(见 extwasm 的 `Limits::engine`)。
    #[serde(default)]
    pub engine: bool,
    /// 激活方式:触发词、建议热键与命令。
    #[serde(default)]
    pub activate: Option<Activate>,
    /// 需要的宿主服务。宿主把全部服务接口链接进组件(非 Rust 工具链不
    /// 裁剪导入),是否放行按调用逐次门控:没声明的服务一调即拒。
    /// 用户授权页也按这里声明展示。
    #[serde(default)]
    pub needs: Vec<Service>,
    /// 订阅的宿主事件;只有原生扩展能订阅,宿主只推送,不接收回调。
    #[serde(default)]
    pub events: Vec<Event>,
    /// 申请的权限;授权页与同意记录用。
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 联网白名单;`needs` 声明 `network` 时必填。
    #[serde(default)]
    pub network: Option<NetworkGrant>,
    /// 声明式设置:宿主渲染设置页,值存在 `[ext.<id>]`。
    #[serde(default)]
    pub settings: Vec<Setting>,
}

/// 联网白名单:扩展声明它能访问的域名,用户同意后由宿主提供者逐请求
/// 强制。条目是 `host` 或 `host:port`,没有 scheme 与路径;不带端口的
/// 条目只放行该 scheme 的默认端口(https 443、http 80)。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkGrant {
    /// 允许访问的域名,1 到 32 条,精确匹配。
    pub allow: Vec<String>,
}

/// 一条声明式设置。设置页按 `type` 渲染,值落在 `config.toml` 的
/// `[ext.<id>]` 表;`secureText` 的值不进配置文件,存到用户私密目录。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    /// 配置键;在扩展命名空间里唯一。
    pub key: String,
    /// 设置页显示的文字。
    pub title: String,
    /// 设置页的说明;可空。
    #[serde(default)]
    pub detail: String,
    /// `toggle`、`choice` 或 `secureText`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 默认值:toggle 为布尔,choice/secureText 为文本。
    #[serde(default)]
    pub default: Option<SettingValue>,
    /// choice 的选项;其它类型必须为空。
    #[serde(default)]
    pub options: Vec<String>,
}

/// 设置值。JSON 里就是布尔或字符串,配置里也是这两种。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SettingValue {
    /// 开关值。
    Bool(bool),
    /// 文本值。
    Text(String),
}

impl SettingValue {
    /// 从配置里的 JSON 值取;数字、数组、null 都不算设置值。
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        value
            .as_bool()
            .map(Self::Bool)
            .or_else(|| value.as_str().map(|text| Self::Text(text.to_string())))
    }

    /// 写进配置值的 JSON 形态。
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(on) => serde_json::Value::Bool(*on),
            Self::Text(text) => serde_json::Value::String(text.clone()),
        }
    }
}

impl Setting {
    /// 配置键。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 是否是开关。
    pub fn is_toggle(&self) -> bool {
        self.kind == "toggle"
    }

    /// 是否是密钥文本;值存到用户私密目录,不进 `config.toml`。
    pub fn is_secret(&self) -> bool {
        self.kind == "secureText"
    }

    /// 是否是可以自由输入的文本(密钥输入在设置窗里做)。
    pub fn is_text(&self) -> bool {
        self.is_secret()
    }

    /// 单选选项;开关与文本为空。
    pub fn options(&self) -> &[String] {
        &self.options
    }

    /// 清单声明的默认值;写错的值在加载期已被拒绝,这里只兜底。
    pub fn default_value(&self) -> SettingValue {
        if self.is_toggle() {
            return match &self.default {
                Some(SettingValue::Bool(on)) => SettingValue::Bool(*on),
                _ => SettingValue::Bool(false),
            };
        }
        if self.is_text() {
            return match &self.default {
                Some(SettingValue::Text(text)) => SettingValue::Text(text.clone()),
                _ => SettingValue::Text(String::new()),
            };
        }
        match &self.default {
            Some(SettingValue::Text(text)) if self.options.contains(text) => {
                SettingValue::Text(text.clone())
            }
            _ => SettingValue::Text(self.options.first().cloned().unwrap_or_default()),
        }
    }

    /// 配置里的当前值;缺失或类型/选项不符时退回默认值。
    pub fn resolve(&self, stored: Option<&serde_json::Value>) -> SettingValue {
        let value = stored.and_then(SettingValue::from_json);
        if self.is_toggle() {
            return match value {
                Some(SettingValue::Bool(on)) => SettingValue::Bool(on),
                _ => self.default_value(),
            };
        }
        if self.is_text() {
            return match value {
                Some(SettingValue::Text(text)) if text.chars().count() <= MAX_TEXT_CHARS => {
                    SettingValue::Text(text)
                }
                _ => self.default_value(),
            };
        }
        match value {
            Some(SettingValue::Text(text)) if self.options.contains(&text) => {
                SettingValue::Text(text)
            }
            _ => self.default_value(),
        }
    }

    /// 值是否符合这条设置的 schema。
    pub fn accepts(&self, value: &serde_json::Value) -> bool {
        match SettingValue::from_json(value) {
            Some(SettingValue::Bool(_)) => self.is_toggle(),
            Some(SettingValue::Text(text)) if self.is_text() => {
                text.chars().count() <= MAX_TEXT_CHARS && !text.chars().any(char::is_control)
            }
            Some(SettingValue::Text(text)) => !self.is_toggle() && self.options.contains(&text),
            None => false,
        }
    }
}

/// 激活声明。三种入口都只是声明,按键绑定仍归用户。
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

/// 宿主服务。扩展声明需要什么,宿主按需链接;没声明就加载失败。
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
    /// 扩展私有存储目录。
    Store,
    /// 系统通知。
    Notify,
    /// 经独立 broker 进程访问清单白名单里的域名;server 保持离线。
    Network,
    /// 读当前窗口的选中文字与上下文;每级单独同意。
    Context,
    /// 录制麦克风音频。
    Audio,
    /// 离线语音转文字。
    Asr,
}

impl Service {
    /// 全部服务;设置页与诊断按这个顺序列。
    pub const ALL: [Self; 12] = [
        Self::Capture,
        Self::Ocr,
        Self::Clipboard,
        Self::Panel,
        Self::Output,
        Self::Hotkey,
        Self::Store,
        Self::Notify,
        Self::Network,
        Self::Context,
        Self::Audio,
        Self::Asr,
    ];

    /// 清单里的服务名;宿主导入的函数名是 `lueying_<服务名>`。
    pub fn key(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Ocr => "ocr",
            Self::Clipboard => "clipboard",
            Self::Panel => "panel",
            Self::Output => "output",
            Self::Hotkey => "hotkey",
            Self::Store => "store",
            Self::Notify => "notify",
            Self::Network => "network",
            Self::Context => "context",
            Self::Audio => "audio",
            Self::Asr => "asr",
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
            Self::Store => "读写扩展存储",
            Self::Notify => "系统通知",
            Self::Network => "访问外部网络(高敏感)",
            Self::Context => "读取窗口上下文(高敏感)",
            Self::Audio => "录制麦克风(高敏感)",
            Self::Asr => "离线语音识别",
        }
    }

    /// 这个服务对应的权限;没有对应权限的服务单独列进授权说明。
    pub fn permission(self) -> Option<Permission> {
        match self {
            Self::Capture => Some(Permission::ScreenCapture),
            Self::Ocr => Some(Permission::Ocr),
            Self::Clipboard => Some(Permission::Clipboard),
            Self::Panel
            | Self::Output
            | Self::Hotkey
            | Self::Store
            | Self::Notify
            | Self::Network => None,
            Self::Context => Some(Permission::SelectedText),
            Self::Audio => Some(Permission::Audio),
            Self::Asr => None,
        }
    }
}

/// 宿主推给原生扩展的事件。扩展只读,不能借它回调宿主。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Event {
    /// 候选窗状态:显示与否、位置与组合内容(文字需要 candidate-text 权限)。
    CandidateState,
}

/// 扩展能申请的权限。默认一个都不给。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// 读取候选文字。
    CandidateText,
    /// 截取屏幕区域。
    ScreenCapture,
    /// 对截取内容做文字识别。
    Ocr,
    /// 读写剪贴板。
    Clipboard,
    /// 读取当前窗口里用户选中的文字。
    SelectedText,
    /// 录制麦克风。
    Audio,
}

impl Permission {
    /// 全部权限;设置页与诊断按这个顺序列。
    pub const ALL: [Self; 6] = [
        Self::CandidateText,
        Self::ScreenCapture,
        Self::Ocr,
        Self::Clipboard,
        Self::SelectedText,
        Self::Audio,
    ];

    /// 清单里的权限名。
    pub fn key(self) -> &'static str {
        match self {
            Self::CandidateText => "candidate-text",
            Self::ScreenCapture => "screen-capture",
            Self::Ocr => "ocr",
            Self::Clipboard => "clipboard",
            Self::SelectedText => "selected-text",
            Self::Audio => "audio",
        }
    }

    /// 授权页上的人话与敏感级别。读拼音原文是低敏感,读左邻/屏幕内容是高敏感。
    pub fn title(self) -> &'static str {
        match self {
            Self::CandidateText => "读取候选与上下文文字(高敏感)",
            Self::ScreenCapture => "截取屏幕(高敏感)",
            Self::Ocr => "识别文字",
            Self::Clipboard => "读写剪贴板(高敏感)",
            Self::SelectedText => "读取当前窗口选中的文字(高敏感)",
            Self::Audio => "录制麦克风(高敏感)",
        }
    }
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
        if self.api != EXT_API {
            return Err(format!("清单 API {} 不支持,当前支持 {EXT_API}", self.api));
        }
        match self.kind {
            Kind::Wasm => {
                if !self.exe.is_empty() {
                    return Err("kind=wasm 清单不能声明 exe".into());
                }
                check_entry("Wasm 模块", &self.wasm, "wasm")?;
            }
            Kind::Native => {
                if !self.wasm.is_empty() {
                    return Err("kind=native 清单不能声明 wasm 模块".into());
                }
                if self.engine {
                    return Err("kind=native 清单不能声明 engine".into());
                }
                if !self.needs.is_empty() {
                    return Err("原生扩展暂不接宿主服务,清单不能声明 needs".into());
                }
                if self.activate.is_some() {
                    return Err("原生扩展是服务提供者,清单不能声明 activate".into());
                }
                check_entry("可执行文件", &self.exe, "exe")?;
            }
        }
        check_settings(&self.settings)?;
        if !self.capabilities.is_empty() {
            check_capabilities(&self.capabilities)?;
        }
        check_services(&self.needs)?;
        if !self.events.is_empty() && self.kind != Kind::Native {
            return Err("只有原生扩展能订阅宿主事件".into());
        }
        check_network(self)?;
        if let Some(activate) = &self.activate {
            activate.validate()?;
        }
        Ok(())
    }

    /// Wasm 模块,包内裸文件名;仅 [`Kind::Wasm`] 清单可调。
    pub fn wasm_main(&self) -> &str {
        &self.wasm
    }

    /// 主内容文件名:Wasm 模块或原生可执行文件。同意指纹对它算哈希。
    pub fn payload_file(&self) -> &str {
        match self.kind {
            Kind::Wasm => &self.wasm,
            Kind::Native => &self.exe,
        }
    }

    /// 是否原生扩展(独立进程的服务提供者)。
    pub fn is_native(&self) -> bool {
        self.kind == Kind::Native
    }

    /// 是否声明了候选窗外观。
    pub fn is_appearance(&self) -> bool {
        self.appearance
    }

    /// 激活声明;没有返回 `None`。
    pub fn activate(&self) -> Option<&Activate> {
        self.activate.as_ref()
    }

    /// 命令声明;没有激活声明返回空表。
    pub fn commands(&self) -> &[Command] {
        self.activate
            .as_ref()
            .map(|activate| activate.commands.as_slice())
            .unwrap_or(&[])
    }

    /// 是否有需要宿主链接的服务。
    pub fn needs(&self, service: Service) -> bool {
        self.needs.contains(&service)
    }

    /// 是否订阅了某个宿主事件。
    pub fn subscribes(&self, event: Event) -> bool {
        self.events.contains(&event)
    }

    /// 是否申请了 `candidate-text` 权限,即能否收到候选文字。
    pub fn may_read_candidate_text(&self) -> bool {
        self.capabilities
            .iter()
            .any(|cap| cap == Permission::CandidateText.key())
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

/// 声明式设置:键、标题、类型与默认值都要能直接渲染。
fn check_settings(settings: &[Setting]) -> Result<(), String> {
    if settings.len() > 32 {
        return Err(format!("声明了 {} 个设置,上限 32 个", settings.len()));
    }
    let mut keys: Vec<&str> = Vec::new();
    for setting in settings {
        if !valid_id(&setting.key) {
            return Err(format!(
                "设置键 {:?} 不合法:只允许小写字母、数字、- 与 _,且以字母或数字开头",
                setting.key
            ));
        }
        if keys.contains(&setting.key.as_str()) {
            return Err(format!("设置键 {} 重复声明", setting.key));
        }
        keys.push(&setting.key);
        let title_len = setting.title.trim().chars().count();
        if title_len == 0 || setting.title.chars().count() > 80 {
            return Err(format!("设置 {} 的标题为空或超过 80 字", setting.key));
        }
        if setting.title.chars().any(char::is_control)
            || setting.detail.chars().count() > 200
            || setting.detail.chars().any(char::is_control)
        {
            return Err(format!(
                "设置 {} 的标题或说明含控制字符/说明超过 200 字",
                setting.key
            ));
        }
        match setting.kind.as_str() {
            "toggle" => {
                if !setting.options.is_empty() {
                    return Err(format!("开关设置 {} 不能声明 options", setting.key));
                }
                if matches!(&setting.default, Some(SettingValue::Text(_))) {
                    return Err(format!("开关设置 {} 的默认值必须是布尔", setting.key));
                }
            }
            "secureText" => {
                if !setting.options.is_empty() {
                    return Err(format!("密钥设置 {} 不能声明 options", setting.key));
                }
                if let Some(SettingValue::Bool(_)) = &setting.default {
                    return Err(format!("密钥设置 {} 的默认值必须是文本", setting.key));
                }
                if let Some(SettingValue::Text(text)) = &setting.default
                    && (text.chars().count() > MAX_TEXT_CHARS || text.chars().any(char::is_control))
                {
                    return Err(format!("密钥设置 {} 的默认值过长或含控制字符", setting.key));
                }
            }
            "choice" => {
                if setting.options.is_empty() || setting.options.len() > 8 {
                    return Err(format!("单选设置 {} 的选项数必须在 1..=8", setting.key));
                }
                let mut seen: Vec<&str> = Vec::new();
                for option in &setting.options {
                    if option.is_empty()
                        || option.chars().count() > 32
                        || option.chars().any(char::is_control)
                    {
                        return Err(format!(
                            "单选设置 {} 的选项 {option:?} 为空、超过 32 字或含控制字符",
                            setting.key
                        ));
                    }
                    if seen.contains(&option.as_str()) {
                        return Err(format!("单选设置 {} 的选项 {option:?} 重复", setting.key));
                    }
                    seen.push(option);
                }
                match &setting.default {
                    Some(SettingValue::Bool(_)) => {
                        return Err(format!("单选设置 {} 的默认值必须是文本", setting.key));
                    }
                    Some(SettingValue::Text(text)) if !setting.options.contains(text) => {
                        return Err(format!(
                            "单选设置 {} 的默认值 {text:?} 不在选项里",
                            setting.key
                        ));
                    }
                    _ => {}
                }
            }
            other => {
                return Err(format!(
                    "设置 {} 的 type {other:?} 不支持,只能是 toggle、choice 或 secureText",
                    setting.key
                ));
            }
        }
    }
    Ok(())
}

/// id 只允许小写字母、数字、`-`、`_`,以字母或数字开头,最长 48 字节。
pub fn valid_id(id: &str) -> bool {
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

/// 能力名必须在已知权限里,且不能重复。
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

/// 联网白名单的完整性与域名格式;白名单是同意书的一部分,改一个域名
/// 就要重新同意。
fn check_network(manifest: &Manifest) -> Result<(), String> {
    let declared = manifest.needs.contains(&Service::Network);
    match (&manifest.network, declared) {
        (Some(_), false) => {
            return Err("清单声明 network 白名单前,needs 里要先有 network".into());
        }
        (None, true) => {
            return Err("needs 声明 network 就要给出 network.allow 域名白名单".into());
        }
        _ => {}
    }
    let Some(grant) = &manifest.network else {
        return Ok(());
    };
    if grant.allow.is_empty() {
        return Err("network.allow 不能为空;没有可访问的域名就别声明 network".into());
    }
    if grant.allow.len() > 32 {
        return Err("network.allow 超过 32 条上限".into());
    }
    for (index, entry) in grant.allow.iter().enumerate() {
        check_allow_host(entry)?;
        if grant.allow[..index]
            .iter()
            .any(|other| other.eq_ignore_ascii_case(entry))
        {
            return Err(format!("白名单条目 {entry:?} 重复"));
        }
    }
    Ok(())
}

/// 白名单条目:`host` 或 `host:port`,精确匹配才可审计,不收通配符。
/// IPv6 字面量带方括号(如 `[::1]:8080`)。
fn check_allow_host(entry: &str) -> Result<(), String> {
    let bad = |reason: &str| format!("白名单条目 {entry:?} {reason}");
    let (host, port) = if let Some(body) = entry.strip_prefix('[') {
        let Some(end) = body.find(']') else {
            return Err(bad("IPv6 缺右括号"));
        };
        let (v6, rest) = (&body[..end], &body[end + 1..]);
        match rest.strip_prefix(':') {
            Some(port) => (v6, Some(port)),
            None if rest.is_empty() => (v6, None),
            None => return Err(bad("应为 [IPv6]:端口")),
        }
    } else {
        match entry.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (entry, None),
        }
    };
    if let Some(port) = port {
        port.parse::<u16>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| bad("端口要落在 1..=65535"))?;
    }
    if host.is_empty() {
        return Err(bad("缺主机名"));
    }
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    if host.contains(':') {
        if !host.chars().all(|c| ok_char(c) || c == ':') {
            return Err(bad("IPv6 字符不合法"));
        }
    } else if host.len() > 253 || !host.chars().all(ok_char) {
        return Err(bad("只接受主机名或 IPv4,不收通配符与 scheme"));
    } else if host.split('.').any(|label| {
        label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-')
    }) {
        return Err(bad("主机名标签不合法"));
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

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;

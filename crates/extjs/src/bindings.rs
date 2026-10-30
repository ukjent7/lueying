//! 用户绑定表:热键到命令的映射由宿主管理,扩展清单只给建议。
//!
//! 绑定组合用 `Ctrl+Alt+O` 这样的串声明,解析时归一(修饰键顺序、大小写、
//! 键名拼写)。宿主强制三条:至少两个修饰键(挡掉 Alt+Tab、Win+L 这类单
//! 修饰系统键,也挡掉输入法自己的开关组合)、不碰保留组合、同一组合表内
//! 唯一。命中是否真的派发由 [`Input::allows`] 按命令声明的 when 过滤。
//!
//! `Ctrl+Alt+字母` 在部分键盘布局上是 AltGr,绑定这类组合会抢走对应字符
//! 的输入;保留组合名单随实测扩充。

use crate::manifest::When;

/// 修饰键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    /// Ctrl。
    Ctrl,
    /// Alt。
    Alt,
    /// Shift。
    Shift,
    /// Win。
    Win,
}

impl Modifier {
    /// 规范拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
            Self::Win => "Win",
        }
    }
}

/// 解析过的组合:修饰键集合与主键。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// 修饰键;解析时去重,顺序无关。
    pub modifiers: Vec<Modifier>,
    /// 主键;规范拼写。
    pub key: String,
}

impl Hotkey {
    /// 规范组合串:修饰键固定 Ctrl、Alt、Shift、Win 顺序,同义输入归一。
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        for modifier in [
            Modifier::Ctrl,
            Modifier::Alt,
            Modifier::Shift,
            Modifier::Win,
        ] {
            if self.modifiers.contains(&modifier) {
                out.push_str(modifier.as_str());
                out.push('+');
            }
        }
        out.push_str(&self.key);
        out
    }
}

/// 命名键的规范拼写;服务端的虚拟键映射用同一套名字。
const NAMED_KEYS: [&str; 15] = [
    "Space",
    "Enter",
    "Tab",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
    "Esc",
];

/// 系统保留组合;单修饰键的系统键(Alt+Tab、Win+L 等)由两修饰键规则挡住。
const RESERVED: [&str; 7] = [
    "Ctrl+Alt+Del",
    "Ctrl+Shift+Esc",
    "Ctrl+Win+Space",
    "Ctrl+Win+Left",
    "Ctrl+Win+Right",
    "Ctrl+Win+D",
    "Ctrl+Win+F4",
];

/// 解析绑定组合;归一并校验,保留组合在这里拒绝。
pub fn parse(hotkey: &str) -> Result<Hotkey, String> {
    let mut modifiers: Vec<Modifier> = Vec::new();
    let mut key: Option<String> = None;
    for token in hotkey.split('+') {
        let token = token.trim();
        if token.is_empty() {
            return Err(format!("热键 {hotkey:?} 有空段"));
        }
        let modifier = match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(Modifier::Ctrl),
            "alt" => Some(Modifier::Alt),
            "shift" => Some(Modifier::Shift),
            "win" | "windows" => Some(Modifier::Win),
            _ => None,
        };
        match modifier {
            Some(modifier) => {
                if modifiers.contains(&modifier) {
                    return Err(format!("热键 {hotkey:?} 重复了 {}", modifier.as_str()));
                }
                modifiers.push(modifier);
            }
            None => {
                if key.is_some() {
                    return Err(format!("热键 {hotkey:?} 有多个主键"));
                }
                key = Some(canonical_key(token, hotkey)?);
            }
        }
    }
    let Some(key) = key else {
        return Err(format!("热键 {hotkey:?} 缺主键"));
    };
    if modifiers.len() < 2 {
        return Err(format!("热键 {hotkey:?} 至少要两个修饰键"));
    }
    let parsed = Hotkey { modifiers, key };
    let canonical = parsed.canonical();
    if RESERVED.contains(&canonical.as_str()) {
        return Err(format!("热键 {canonical} 是系统保留组合"));
    }
    Ok(parsed)
}

/// 主键的规范拼写:F 键与命名键大小写不敏感,其余只收单个可见字符。
fn canonical_key(token: &str, whole: &str) -> Result<String, String> {
    if let Some(number) = token
        .strip_prefix(['F', 'f'])
        .and_then(|rest| rest.parse::<u32>().ok())
        && (1..=12).contains(&number)
        && token.len() <= 3
    {
        return Ok(format!("F{number}"));
    }
    for name in NAMED_KEYS {
        if token.eq_ignore_ascii_case(name) {
            return Ok(name.to_string());
        }
    }
    let mut chars = token.chars();
    let (Some(first), None) = (chars.next(), chars.next()) else {
        return Err(format!("热键 {whole:?} 的主键 {token:?} 不合法"));
    };
    if !first.is_ascii_graphic() {
        return Err(format!("热键 {whole:?} 的主键 {token:?} 不合法"));
    }
    Ok(first.to_ascii_uppercase().to_string())
}

/// 一条绑定:规范组合串 → 完整命令 id 与生效条件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// 规范组合串,如 `Ctrl+Alt+O`。
    pub hotkey: String,
    /// 完整命令 id `<扩展 id>.<命令 id>`。
    pub command: String,
    /// 绑定生效的输入状态;取命令声明。
    pub when: When,
}

/// 触发时刻的输入状态快照。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
    /// 焦点窗口可编辑(有输入法会话)。
    pub editable: bool,
    /// 正在组句。
    pub composing: bool,
}

impl Input {
    /// when 在当前状态下是否生效;组句蕴含可编辑。
    pub fn allows(self, when: When) -> bool {
        match when {
            When::Any => true,
            When::Editable => self.editable || self.composing,
            When::Composing => self.composing,
        }
    }
}

/// 用户绑定表;宿主持有,扩展只能建议。
#[derive(Clone, Debug, Default)]
pub struct BindingTable {
    rows: Vec<Binding>,
}

impl BindingTable {
    /// 校验并加入一条绑定;组合串归一,保留组合与表内冲突在这里拒绝。
    pub fn insert(&mut self, hotkey: &str, command: String, when: When) -> Result<(), String> {
        let parsed = parse(hotkey)?;
        let canonical = parsed.canonical();
        if let Some(existing) = self.rows.iter().find(|row| row.hotkey == canonical) {
            return Err(format!("热键 {canonical} 已绑定 {}", existing.command));
        }
        self.rows.push(Binding {
            hotkey: canonical,
            command,
            when,
        });
        Ok(())
    }

    /// 全部绑定;下标即注册行号,触发事件按行号回带。
    pub fn rows(&self) -> &[Binding] {
        &self.rows
    }

    /// 命中组合且当前状态允许的绑定;测试与诊断用。
    pub fn hit(&self, hotkey: &str, input: Input) -> Option<&Binding> {
        let parsed = parse(hotkey).ok()?;
        let canonical = parsed.canonical();
        self.rows
            .iter()
            .find(|row| row.hotkey == canonical && input.allows(row.when))
    }
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod bindings_tests;

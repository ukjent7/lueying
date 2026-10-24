//! TSF DLL 与 Server 进程间的协议。传输为命名管道上的行 JSON(每行一条,
//! 请求-应答串行,调试器可直接抓管道看明文)。

use serde::{Deserialize, Serialize};

/// 管道名(版本号入名,协议不兼容时换 v2)。
pub const PIPE_NAME: &str = r"\\.\pipe\pinyin-ime-v1";

/// 带每用户后缀的管道名:用户名哈希入名,防止跨用户会话抢注同名管道
/// (mozc 的 IPCKey 防劫持方案的轻量版;两端同会话同环境变量,得到同一名字)。
pub fn pipe_name() -> String {
    match std::env::var("USERNAME") {
        Ok(user) if !user.is_empty() => {
            let hash = user
                .bytes()
                .fold(5381u64, |h, b| h.wrapping_mul(33).wrapping_add(b as u64));
            format!("{PIPE_NAME}.u{hash:x}")
        }
        _ => PIPE_NAME.to_string(),
    }
}

/// 线上的按键(引擎 `Key` 的可序列化镜像,引擎 crate 保持零依赖)。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyMsg {
    Letter(char),
    Digit(u8),
    Space,
    Enter,
    Esc,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Punct(char),
    Shift,
}

impl From<KeyMsg> for pinyin_engine::Key {
    fn from(k: KeyMsg) -> Self {
        use pinyin_engine::Key;
        match k {
            KeyMsg::Letter(c) => Key::Letter(c),
            KeyMsg::Digit(d) => Key::Digit(d),
            KeyMsg::Space => Key::Space,
            KeyMsg::Enter => Key::Enter,
            KeyMsg::Esc => Key::Esc,
            KeyMsg::Backspace => Key::Backspace,
            KeyMsg::Delete => Key::Delete,
            KeyMsg::Left => Key::Left,
            KeyMsg::Right => Key::Right,
            KeyMsg::Up => Key::Up,
            KeyMsg::Down => Key::Down,
            KeyMsg::Home => Key::Home,
            KeyMsg::End => Key::End,
            KeyMsg::PageUp => Key::PageUp,
            KeyMsg::PageDown => Key::PageDown,
            KeyMsg::Punct(c) => Key::Punct(c),
            KeyMsg::Shift => Key::Shift,
        }
    }
}

/// DLL → Server。
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// 会话上线(会话号 = 宿主线程 id;host 为宿主 exe 名,兼容性判断用)。
    Hello { session: u64, host: String },
    /// 一次按键(TSF 的 TestKeyDown 阶段送达;Server 无副作用地预演,
    /// 结果缓存在会话里,随后的 `TakeKey` 取走写文档)。
    Key { session: u64, key: KeyMsg },
    /// 取走上次 `Key` 的结果(OnKeyDown 时)。
    TakeKey { session: u64 },
    /// 停顿轮询:只读当前内联文本,供 TSF 定时器把异步重排后的首选写回文档。
    Sync { session: u64 },
    /// DLL 侧直接设置模式(CapsLock 大写锁定等)。
    SetMode { session: u64, chinese: bool },
    /// 焦点变化(切窗口时 Server 收候选窗)。
    Focus { session: u64, focused: bool },
    /// 应用强行终止了组句。
    CompositionTerminated { session: u64 },
    /// 组句/光标的屏幕矩形(编辑会话里量得,候选窗定位用)。
    Caret {
        session: u64,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    },
    /// 会话下线。
    Goodbye { session: u64 },
}

/// Server → DLL。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Reply {
    /// 要写进文档的上屏文本(按序)。
    pub commits: Vec<String>,
    /// 组合内联文本(空 = 无组合)。
    pub preedit: String,
    /// 当前是否中文模式(语言栏指示用)。
    pub chinese: bool,
    /// 键应直通给应用。
    pub passthrough: bool,
}

/// 会话快照(Server 内部与候选窗渲染共用)。
#[derive(Debug, Default, Clone)]
pub struct SessionView {
    pub preedit: String,
    /// 分段显示的拼音。
    pub segments: Vec<String>,
    pub candidates: Vec<String>,
    pub page: usize,
    pub page_size: usize,
    pub highlight: usize,
    pub chinese: bool,
    pub caret: Option<(i32, i32, i32, i32)>,
    pub host: String,
}

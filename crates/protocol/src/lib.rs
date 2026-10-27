//! TSF DLL 与 Server 进程间的协议。传输为命名管道上的行 JSON(每行一条,
//! 请求-应答串行,调试器可直接抓管道看明文)。

use serde::{Deserialize, Serialize};

#[cfg(windows)]
mod spawn;
#[cfg(windows)]
pub use spawn::spawn_detached;
pub mod version;

/// 管道名(版本号入名,协议不兼容时换 v2)。
pub const PIPE_NAME: &str = r"\\.\pipe\lueying-v1";

/// 带每用户、每会话后缀的管道名:用户名哈希 + 登录会话号入名。
///
/// 只按用户区分时,RDP/快速用户切换下同一用户的两个会话会连到同一个
/// Server,候选窗可能出现在另一个桌面。会话号由系统分配,两端在同一
/// 会话里得到同一个名字。
///
/// `LUEYING_PIPE_SUFFIX` 只给自动化测试隔离用,避免连到本机已安装的服务。
pub fn pipe_name() -> String {
    if let Ok(suffix) = std::env::var("LUEYING_PIPE_SUFFIX") {
        let clean: String = suffix
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(16)
            .collect();
        if !clean.is_empty() {
            return format!("{PIPE_NAME}.t{clean}");
        }
    }
    let user = match std::env::var("USERNAME") {
        Ok(user) if !user.is_empty() => {
            let hash = user
                .bytes()
                .fold(5381u64, |h, b| h.wrapping_mul(33).wrapping_add(b as u64));
            format!(".u{hash:x}")
        }
        _ => String::new(),
    };
    format!("{PIPE_NAME}{user}.s{}", current_session())
}

/// 当前进程所属的 Windows 登录会话号;非 Windows 构建固定为 0。
fn current_session() -> u32 {
    #[cfg(windows)]
    {
        use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
        let mut session = 0u32;
        // 查询失败时退回 0:最坏情况是两个会话共用名字,不影响单会话输入。
        unsafe {
            let _ = ProcessIdToSessionId(std::process::id(), &mut session);
        }
        session
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// 线上的按键。协议 crate 只描述管道格式,不依赖引擎;转换在 Server 边界做。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyMsg {
    Letter(char),
    Digit(u8),
    Space,
    Enter,
    Esc,
    Backspace,
    Delete,
    SegmentBackspace,
    SegmentDelete,
    Left,
    Right,
    SegmentLeft,
    SegmentRight,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    DeleteCandidate,
    Punct(char),
    Shift,
}

/// DLL → Server。
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// 会话上线(会话号 = 宿主进程 id 高 32 位 + 线程 id 低 32 位;
    /// host 为宿主 exe 名,兼容性判断用)。
    Hello { session: u64, host: String },
    /// 一次按键(TSF 的 TestKeyDown 阶段送达;Server 无副作用地预演,
    /// 结果缓存在会话里,随后的 `TakeKey` 取走写文档)。
    Key { session: u64, key: KeyMsg },
    /// 取走上次 `Key` 的结果(OnKeyDown 时)。
    TakeKey { session: u64 },
    /// 停顿轮询:只读当前内联文本,供 TSF 定时器把异步重排后的首选写回文档。
    Sync { session: u64 },
    /// TSF 读到的光标前文本;组合开始时上报,让重排从第一个候选就有上下文。
    /// `private` 来自输入范围 `IS_PRIVATE`(私密字段),只影响学习,不影响输入。
    /// `fallback` 表示 TSF 没读到可信文本(Office/自绘宿主),Server 可改用 UIA;
    /// `hwnd` 是焦点窗口,`epoch` 是焦点代次,异步 UIA 结果回来时据此丢弃过期文本。
    Context {
        session: u64,
        text: String,
        #[serde(default)]
        private: bool,
        #[serde(default)]
        fallback: bool,
        #[serde(default)]
        hwnd: u64,
        #[serde(default)]
        epoch: u64,
    },
    /// DLL 侧直接设置模式(CapsLock 大写锁定、系统转换模式等)。
    SetMode { session: u64, chinese: bool },
    /// 语言栏/右键菜单切中英;不受「Shift 单击切换」开关限制。
    ToggleMode { session: u64 },
    /// Shift+Space 切换全角/半角;全局状态,不写配置。
    ToggleFullwidth { session: u64 },
    /// 系统「转换模式」compartment 的 FULLSHAPE 位变化。
    SetFullwidth { session: u64, fullwidth: bool },
    /// 设置窗保存后通知 Server 重新读配置文件。
    ReloadConfig,
    /// 保留键 Ctrl+.：切换中文标点并持久化。
    TogglePunct,
    /// 设置窗清空共享用户词库(所有会话立即生效)。
    ClearUser,
    /// 设置窗导出用户词库文本。
    ExportUser,
    /// 设置窗导入用户词库文本(与现有词条合并)。
    ImportUser { text: String },
    /// 设置窗读取模糊组学习统计。
    FuzzyStats,
    /// 设置窗只重置组级适应,保留词条。
    ResetFuzzy,
    /// 安装器卸载前让常驻服务干净退出。
    Shutdown,
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
    /// 候选窗鼠标点选:当前页内 0 基序号。
    Pick { session: u64, slot: usize },
    /// 候选窗右键置顶:当前页内 0 基序号,该键下此后直接放首位。
    Pin { session: u64, slot: usize },
    /// 候选窗右键降权:当前页内 0 基序号,同键记一次跳过负分。
    Demote { session: u64, slot: usize },
    /// 宿主自绘候选时按全局候选序号点选。
    PickIndex { session: u64, index: usize },
    /// 宿主要求原样落定当前显示文本(`FinalizeExactCompositionString`)。
    Finalize { session: u64 },
    /// 宿主自绘候选时移动高亮(不提交)。
    SelectIndex { session: u64, index: usize },
    /// 候选窗滚轮翻页:`delta` 为 ±1。
    Page { session: u64, delta: i32 },
    /// 宿主经 TSF `BeginUIElement`/`Show` 接管了候选绘制;Server 收起自己的窗口。
    UiMode { session: u64, host_draws: bool },
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
    /// 当前是否全角模式(语言栏/模式徽标用)。
    #[serde(default)]
    pub fullwidth: bool,
    /// 键应直通给应用。
    pub passthrough: bool,
    /// 模糊组统计(仅设置窗的 `FuzzyStats`/`ResetFuzzy` 应答使用)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<(String, u32)>,
    /// 用户词库文本(仅 `ExportUser` 应答使用)。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// 全部候选(按页切片由 `page`/`page_size` 决定),供 TSF 的
    /// `ITfCandidateListUIElement` 回答宿主与读屏软件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// 当前页号与每页候选数(候选 UI 元素的分页信息)。
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    pub page_size: usize,
    /// 全局候选序号中的高亮项。
    #[serde(default)]
    pub highlight: usize,
}

/// 会话快照(Server 内部与候选窗渲染共用)。
#[derive(Debug, Default, Clone)]
pub struct SessionView {
    /// 组合的原始拼音(含用户输入的隔音符),候选窗首行显示它。
    pub raw: String,
    pub preedit: String,
    /// 分段显示的拼音。
    pub segments: Vec<String>,
    pub candidates: Vec<String>,
    /// 与 `candidates` 一一对应的注音提示;空串表示不显示。
    pub hints: Vec<String>,
    pub page: usize,
    pub page_size: usize,
    pub highlight: usize,
    pub chinese: bool,
    pub fullwidth: bool,
    pub caret: Option<(i32, i32, i32, i32)>,
    pub host: String,
}

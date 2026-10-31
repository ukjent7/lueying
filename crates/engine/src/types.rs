//! 引擎与宿主、功能模块共享的输入 DTO。
//!
//! 这些类型不依赖窗口系统或平台 API;TSF/Server、E2E 与后续功能模块都通过
//! 它们交换按键、事件、配置与组合快照。

use std::collections::BTreeMap;

use lueying_dict::Fuzzy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Chinese,
    English,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Letter(char),
    Digit(u8),
    Space,
    Enter,
    Esc,
    Backspace,
    Delete,
    /// Ctrl+Backspace:删除光标前一个音节。
    SegmentBackspace,
    /// Ctrl+Delete:删除光标后一个音节。
    SegmentDelete,
    Left,
    Right,
    /// Ctrl+Left/Ctrl+Right:按音节移动组合光标。
    SegmentLeft,
    SegmentRight,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// 删除当前高亮候选的学习记录(Shift+Delete);词典词条本身不受影响。
    DeleteCandidate,
    Punct(char),
    Shift,
}

/// 触发词对应的动作:整串输入完全匹配时作为候选出现,选中执行命令,不上屏文字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// 触发词(小写字母串,不含隔音符);只做整串完全匹配,不做前缀匹配。
    pub trigger: String,
    /// 完整命令 id `<扩展 id>.<命令 id>`。
    pub id: String,
    /// 候选上显示的标题。
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// 上屏一段文本。
    Commit(String),
    /// 选中动作候选:执行该命令,不上屏文字。
    Action(String),
    /// 模式切换。
    Mode(Mode),
    /// 键不被输入法处理,交还宿主直通。
    Passthrough(Key),
}

/// 繁体输出字形:通用 OpenCC 标准,或台湾/香港常用字形。地区字形在通用
/// 转换之上二次映射,只改异体字,不做词汇替换。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Script {
    #[default]
    Simplified,
    Traditional,
    Taiwan,
    HongKong,
}

/// 本地民用时间。宿主在按键前注入;纯引擎环境退回 UTC。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// ISO 周几:1 = 星期一 … 7 = 星期日。
    pub weekday: u32,
}

impl CivilTime {
    /// 没有宿主注入时使用;E2E 全部注入固定时间,不受运行机器时区影响。
    pub(crate) fn now_utc() -> Self {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        Self::from_unix(secs)
    }

    fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (rem / 3_600) as u32,
            minute: (rem % 3_600 / 60) as u32,
            second: (rem % 60) as u32,
            // 1970-01-01 是星期四,加 3 后对 7 取余正好落在 ISO 星期一。
            weekday: ((days + 3).rem_euclid(7) + 1) as u32,
        }
    }
}

/// Howard Hinnant 的 civil_from_days:Unix 天数 → 公历年月日。
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub fuzzy: Fuzzy,
    pub chinese_punct: bool,
    pub page_size: usize,
    /// 用户自定义标点:键是单个半角符号,输出任意文本。查表先于默认
    /// 中文标点表;映射固定输出,不做引号交替,也不受「数字后半角」影响。
    pub punct: BTreeMap<char, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 零配置:核心模糊组默认参与召回,排序由处数罚分与学习覆盖负责。
            fuzzy: Fuzzy::auto(),
            chinese_punct: true,
            page_size: 5,
            punct: BTreeMap::new(),
        }
    }
}

/// 组合态快照(UI 用)。
#[derive(Clone, Debug)]
pub struct Composition {
    /// 原始输入(小写字母与隔音符)。
    pub raw: String,
    /// 光标(原始串内字节偏移,内容全 ASCII)。
    pub cursor: usize,
    /// 内联显示文本:首选转换,无候选时原文。
    pub display: String,
    /// 当前切分各音节的原始区间。
    pub seg_spans: Vec<(usize, usize)>,
}

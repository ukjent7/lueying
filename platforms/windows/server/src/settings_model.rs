//! 设置页的数据模型:页面、分组、行与控件描述。
//!
//! 只描述「长什么样、每个控件叫什么」,不碰绘制与几何;Windows 侧把它
//! 转成 Slint 结构渲染,Linux E2E 直接断言这份数据。控件 id 的前缀在
//! [`crate::settings_view::ids`]。

/// 控件标识:同类控件共用前缀,组内按下标区分。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Id {
    /// 前缀:一组同类控件共用。
    pub base: String,
    /// 组内下标。
    pub index: usize,
}

impl Id {
    pub fn new(base: impl Into<String>, index: usize) -> Self {
        Self {
            base: base.into(),
            index,
        }
    }
}

/// 按钮外观。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// 一个按钮。
#[derive(Clone, Debug)]
pub struct Button {
    pub id: Id,
    pub text: String,
    pub kind: ButtonKind,
    pub enabled: bool,
}

/// 分段组里的一格。
#[derive(Clone, Debug)]
pub struct SegmentOption {
    pub text: String,
    /// 选中时的文字颜色;None 用默认色。
    pub tint: Option<u32>,
}

/// 候选窗预览的样式与样例候选。设置页改字号、排列或主题色时,
/// 同一份数据直接重画,不经过真实候选窗进程。
#[derive(Clone, Debug)]
pub struct CandidatePreview {
    /// 强调色。
    pub accent: u32,
    /// 候选文字缩放倍数。
    pub font_scale: f32,
    /// true 为竖排,false 为横排。
    pub vertical: bool,
    /// 每页候选数。
    pub page_size: usize,
    /// 预览里的拼音行。
    pub pinyin: String,
    /// 样例候选。
    pub candidates: Vec<String>,
}

impl SegmentOption {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tint: None,
        }
    }

    /// 带固定文字颜色的选项(主题色一列用)。
    pub fn tinted(text: impl Into<String>, tint: u32) -> Self {
        Self {
            text: text.into(),
            tint: Some(tint),
        }
    }
}

/// 行右侧的控件。
#[derive(Clone, Debug)]
pub enum Control {
    /// 开关。
    Toggle { id: Id, on: bool },
    /// 分段组;每格按下标有自己的 id,`selected` 是选中格。
    Segment {
        id: Id,
        options: Vec<SegmentOption>,
        selected: usize,
        /// 每格宽度(逻辑像素)。
        width: f32,
    },
    /// 一排按钮。
    Buttons { items: Vec<Button> },
    /// 按钮与开关并排:开关贴行右,按钮在它左边。
    ButtonsToggle {
        items: Vec<Button>,
        toggle: Id,
        toggle_on: bool,
    },
    /// 候选窗预览;只画不点,占用比普通行更高的区域。
    Preview { id: Id, style: CandidatePreview },
    /// 右侧没有控件。
    None,
}

/// 一行设置:左边标题与说明,右边一个控件。
#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    pub detail: String,
    pub control: Control,
}

/// 分组:小标题 + 若干行。
#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub rows: Vec<Row>,
}

/// 左栏的一个分类入口。
#[derive(Clone, Debug)]
pub struct NavItem {
    pub id: Id,
    pub text: String,
}

/// 页面:头部、左栏导航、分组与页脚。
#[derive(Clone, Debug)]
pub struct Page {
    pub title: String,
    pub subtitle: String,
    /// 顶部搜索框里的文字。
    pub search: String,
    /// 头部右侧的按钮(检查更新)。
    pub header_button: Option<Button>,
    /// 左栏分类导航。
    pub nav: Vec<NavItem>,
    /// 当前选中的左栏下标。
    pub active_nav: usize,
    pub groups: Vec<Group>,
    /// 页脚提示文字与颜色。
    pub notice: String,
    pub notice_tint: u32,
    /// 页脚右侧的按钮(关闭、保存)。
    pub footer_buttons: Vec<Button>,
}

//! 宿主选择面板:Rune 窗口承载,可获焦,键盘与鼠标选择。
//!
//! 扩展 `panel.show({title, items})` 经 PanelProvider 投递,投递叫醒
//! 面板窗口的 poll(窗口隐藏时 poll 也会跑),显示一单;键盘/鼠标选择后
//! 把下标经 Done 回传,取消回传 `null`。关闭时把焦点还给唤出前的窗口。
//! 面板与候选窗、徽标、工具栏同在一个 Rune App、同一条主线程。

use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use lueying_ext::host::Done;
use lueying_render::panel::{self, Palette};
use lueying_render::picker;
use lueying_server::host_panel::{HostPanel, HostPanelView};
use lueying_server::services::PanelJob;
use lueying_server::theme;
use rune::tok::motion;
use rune::{
    App, AppHandle, AppSpec, Cursor, Cx, El, Key, KeyEvent, TextStyle, Transition, Tx,
    WindowHandle, WindowOptions, WindowSpec, col, el, text,
};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, GetForegroundWindow, GetWindowRect,
    GetWindowTextW, IsWindow, RegisterClassW, SW_SHOW, SetForegroundWindow, ShowWindow,
    WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

use crate::ui::place;

/// 面板窗口的应用句柄;投递端靠它叫醒隐藏中的 poll。
static APP: OnceLock<AppHandle<Picker>> = OnceLock::new();
/// 面板窗口句柄;显示、收起与摆位都走它。
static WINDOW: OnceLock<WindowHandle> = OnceLock::new();
/// 安装时读下的主题色下标;面板窗口的状态构建要用。
static PICKER_ACCENT: OnceLock<usize> = OnceLock::new();
/// 请求通道容量:面板开着时再来的请求直接回「正忙」。
const QUEUE: usize = 1;
/// 卡片圆角;行内圆角沿用 render 模型的 6。
const CARD_RADIUS: f32 = 8.0;

/// 面板窗口的状态;Rune 每帧从它重建控件树。
pub(super) struct Picker {
    /// 请求通道:panel.show 的投递端,由 PanelProvider 持有。
    jobs: Receiver<PanelJob>,
    /// 主题色下标;安装时读一次,与旧窗口行为一致。
    accent: usize,
    appearance: theme::Appearance,
    panel: Option<HostPanel>,
    /// 视图与几何:行矩形以逻辑点给出,控件树按它绝对定位。
    spec: picker::Spec,
    layout: picker::Layout,
    /// 正在显示的一单的完成句柄;收起时回传选择。
    current: Option<Done>,
    /// 显示期间到达的下一单;收起后立刻显示。
    pending: Option<PanelJob>,
    /// 唤出前的前台窗口(裸句柄值);关闭时把焦点还给它。
    restore: isize,
    visible: bool,
    hover: Option<usize>,
    selected: Option<u32>,
}

impl Picker {
    fn new(jobs: Receiver<PanelJob>, accent: usize) -> Self {
        Self {
            appearance: theme::Appearance::of_system(),
            jobs,
            accent,
            panel: None,
            spec: picker::Spec::default(),
            layout: picker::Layout::default(),
            current: None,
            pending: None,
            restore: 0,
            visible: false,
            hover: None,
            selected: None,
        }
    }

    /// 显示一单:记下唤出时的前台窗口,摆位并打开(打开即激活取焦点)。
    fn show(&mut self, job: PanelJob) {
        let PanelJob { panel, done } = job;
        self.restore = foreground();
        self.panel = Some(panel);
        self.current = Some(done);
        self.selected = None;
        self.hover = None;
        self.refresh();
        self.place();
        if let Some(window) = WINDOW.get() {
            window.open();
        }
        self.visible = true;
    }

    /// 按当前模型重算视图与行几何;可见时尺寸变了才重新摆位。
    fn refresh(&mut self) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        self.spec = spec_of(&panel.view(picker::MAX_ROWS));
        let before = self.layout.size;
        self.layout = picker::layout(&self.spec, 1.0);
        if self.visible && self.layout.size != before {
            self.place();
        }
    }

    /// 面板在唤出窗口所在显示器的上半部居中;位置用物理像素,
    /// 尺寸用逻辑点(`resize_instant`),缩放由 Rune 按所在屏换算。
    fn place(&self) {
        let Some(window) = WINDOW.get() else {
            return;
        };
        let (w, h) = (
            self.layout.size.0.round() as i32,
            self.layout.size.1.round() as i32,
        );
        let anchor = (self.restore != 0)
            .then(|| {
                let rect = window_rect(HWND(self.restore as *mut _));
                ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
            })
            .or_else(|| {
                let mut point = POINT::default();
                unsafe { GetCursorPos(&mut point) }.ok()?;
                Some((point.x, point.y))
            });
        let work = anchor
            .and_then(|(x, y)| place::monitor_info_at(x, y))
            .map_or_else(place::primary_work_area, |info| info.rcWork);
        let scale = anchor.map_or(1.0, |(x, y)| place::scale_at(x, y));
        let width = work.right - work.left;
        let height = work.bottom - work.top;
        let x = work.left + (((width - (w as f32 * scale) as i32) / 2).max(0));
        let y = work.top + (((height - (h as f32 * scale) as i32) / 4).max(0));
        window.set_position((x, y));
        window.resize_instant(self.layout.size);
    }

    /// 选中当前行并收起。
    fn choose(&mut self) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        self.selected = Some(panel.selected_index() as u32);
        self.hide(true);
    }

    /// 点到某行就选中那一行并收起;行下标与选中差即绝对移动量。
    fn click(&mut self, index: usize) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let delta = index as isize - self.spec.selected as isize;
        panel.move_by(delta);
        self.choose();
    }

    /// 方向键/滚轮移动选中行;视图卷动跟着走。
    fn move_selection(&mut self, delta: isize) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        if panel.move_by(delta) {
            self.refresh();
        }
    }

    /// 收起面板;`restore` 为真时把焦点还给唤出前的窗口。
    /// 选择立即经 Done 回传(null 表示取消),排队的下一单接着显示。
    fn hide(&mut self, restore: bool) {
        if !self.visible {
            return;
        }
        self.visible = false;
        if let Some(done) = self.current.take() {
            let selected = self.selected.take();
            done.complete(Ok(serde_json::json!({ "selected": selected }).to_string()));
        }
        self.panel = None;
        if let Some(window) = WINDOW.get() {
            window.hide();
        }
        if restore {
            let target = std::mem::replace(&mut self.restore, 0);
            if target != 0 {
                let hwnd = HWND(target as *mut _);
                if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                    let _ = unsafe { SetForegroundWindow(hwnd) };
                }
            }
        } else {
            self.restore = 0;
        }
        if let Some(job) = self.pending.take() {
            self.show(job);
        }
    }
}

/// 装上请求通道:扩展 `panel.show` 投递后叫醒面板窗口的 poll。
/// 返回接收端,交给面板窗口的状态;窗口起来之前投递会在通道里排队。
pub(super) fn install(state: &crate::imp::State) -> Receiver<PanelJob> {
    let (tx, rx) = mpsc::sync_channel::<PanelJob>(QUEUE);
    let accent = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        cfg.ui_accent
    };
    PICKER_ACCENT.set(accent).ok();
    state.panel_service.install(tx, || {
        // 隐藏中的窗口 poll 也会跑;没起来时投递在通道里等。
        if let Some(app) = APP.get() {
            app.wake();
        }
    });
    rx
}

/// 面板窗口的规格:隐藏创建、无边框置顶、不可缩放;
/// poll 排请求,键盘在窗口级 on_key,行与滚轮在控件树上。
/// 失焦即按取消收起:输入法选择面板不钉在前台,也别把焦点抢回唤出窗。
pub(super) fn window_spec(jobs: Receiver<PanelJob>) -> WindowSpec<Picker> {
    let accent = *PICKER_ACCENT.get_or_init(|| 0);
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        on_key: Some(picker_key),
        poll: Some(picker_poll),
        ..AppSpec::new(
            "掠影选择面板",
            (picker::WIDTH, 160.0),
            placeholder,
            picker_view,
        )
    };
    WindowSpec::from_app(
        "picker",
        App::new(spec)
            .init(move || Picker::new(jobs, accent))
            .on_focus(|s: &mut Picker, focused, _| {
                if s.visible && !focused {
                    s.hide(false);
                }
            }),
    )
    .options(WindowOptions {
        transparent: true,
        borderless: true,
        always_on_top: true,
        visible: false,
        resizable: false,
        min_size: Some((0.0, 0.0)),
        ..Default::default()
    })
}

/// 把窗口句柄存进静态;install 的投递端靠它们叫醒与摆位。
pub(super) fn attach(app: AppHandle<Picker>, window: WindowHandle) {
    let _ = APP.set(app);
    let _ = WINDOW.set(window);
}

/// App::init 已接管状态构建;这个 fn 指针只是 AppSpec 的必填占位。
fn placeholder() -> Picker {
    Picker::new(mpsc::channel().1, 0)
}

/// 每帧开头跑一次:排干请求通道。显示中的面板继续显示,排队的下一单
/// 在收起后显示;再多就以「正忙」回绝(与旧窗口的容量 1 一致)。
fn picker_poll(s: &mut Picker, _now: f64, _tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Ok(job) = s.jobs.try_recv() {
        if s.panel.is_none() {
            s.show(job);
            changed = true;
        } else if s.pending.is_none() {
            s.pending = Some(job);
        } else {
            job.done.complete(Err("宿主面板正忙".into()));
        }
    }
    changed
}

/// 窗口级键盘:树里没有可聚焦控件,所有键都到这里。
fn picker_key(s: &mut Picker, event: &KeyEvent, _tx: &mut Tx) -> bool {
    if !s.visible {
        return false;
    }
    match event.key {
        Key::Up => s.move_selection(-1),
        Key::Down => s.move_selection(1),
        Key::PageUp => s.move_selection(-(picker::MAX_ROWS as isize)),
        Key::PageDown => s.move_selection(picker::MAX_ROWS as isize),
        Key::Home => s.move_selection(isize::MIN / 2),
        Key::End => s.move_selection(isize::MAX / 2),
        Key::Enter => s.choose(),
        Key::Escape => s.hide(true),
        _ => return false,
    }
    true
}

/// 一帧控件树:圆角卡片、标题分隔线、行列表与页脚。
fn picker_view(s: &Picker, cx: &mut Cx<Picker>) -> El {
    // 系统关掉"动画效果"时,框架把过渡降级为淡出。
    cx.reduced_motion(!theme::client_area_animation());
    if !s.visible {
        return el();
    }
    let palette = theme::palette(theme::colors_with(s.appearance, s.accent));
    let (w, h) = s.layout.size;
    col()
        .w(w)
        .h(h)
        .radius(CARD_RADIUS)
        .clip_rounded(CARD_RADIUS)
        .fill(theme::srgb(palette.bg))
        .border(1.0, theme::srgb(palette.border))
        .key("panel")
        .transition(Transition::offset(0.0, 4.0).combined(Transition::opacity()))
        .motion(motion::SETTLE)
        .child(title_el(&s.spec, &s.layout, &palette))
        .child(rows_el(s, &palette))
        .child(footer_el(&s.spec, &s.layout, &palette))
}

/// 标题行:标题左,条目总数右,底部 1px 分隔线。
fn title_el(spec: &picker::Spec, layout: &picker::Layout, palette: &Palette) -> El {
    let pad = picker::PAD;
    let title = layout.title;
    let count = format!("{} 项", spec.total);
    let title_budget = title.width() - 3.0 * pad;
    col()
        .w_full()
        .h(title.height())
        .child(
            el().absolute(pad, super::rune::ink_y(title.height(), 15.0))
                .w(title_budget)
                .child(
                    text(ellipsize(&spec.title, title_budget, 15.0))
                        .font(TextStyle::new(15.0, 400.0))
                        .color(theme::srgb(palette.text)),
                ),
        )
        .child(
            el().absolute(
                title.width() - pad - 60.0,
                super::rune::ink_y(title.height(), 11.0),
            )
            .w(60.0)
            .child(
                text(count)
                    .font(TextStyle::new(11.0, 400.0))
                    .color(theme::srgb(palette.text_tertiary))
                    .a11y_hidden(),
            ),
        )
        .child(
            el().absolute(pad, title.height() - 1.0)
                .w(title.width() - 2.0 * pad)
                .h(1.0)
                .fill(theme::srgb(palette.border))
                .a11y_hidden(),
        )
}

/// 行列表:选中行是主题色胶囊,悬停亮底;滚轮翻动。
fn rows_el(s: &Picker, palette: &Palette) -> El {
    let list = s.layout.list;
    let mut rows = col()
        .absolute(0.0, list.top)
        .w(list.width())
        .h(list.height())
        .on_wheel(|s: &mut Picker, e, _| {
            if s.spec.rows.is_empty() {
                return false;
            }
            // dy>0 是向下滚,向前翻三条,与候选窗的滚动方向一致。
            s.move_selection((e.dy.signum() * 3.0) as isize);
            true
        });
    if s.spec.rows.is_empty() {
        rows = rows.child(
            el().w_full().h_full().center().child(
                text("没有可选项")
                    .font(TextStyle::new(14.0, 400.0))
                    .color(theme::srgb(palette.text_secondary))
                    .a11y_hidden(),
            ),
        );
        return rows;
    }
    for (index, row) in s.spec.rows.iter().enumerate() {
        rows = rows.child(row_el(s, index, row.clone(), palette));
    }
    if s.spec.above > 0 || s.spec.below > 0 {
        rows = rows.child(scroll_hints_el(s, palette));
    }
    rows
}

/// 一行:绝对定位在行矩形上,选中是主题色胶囊(纵向收进 2px,与行
/// 留出呼吸,与旧画法一致),悬停亮底,点击选中并收起。
fn row_el(s: &Picker, index: usize, row: String, palette: &Palette) -> El {
    let Some(rect) = s.layout.rows.get(index) else {
        return el();
    };
    let rect = *rect;
    let selected = index == s.spec.selected;
    let hovered = s.hover == Some(index);
    let (top, height) = if selected {
        (rect.top + 2.0, rect.height() - 4.0)
    } else {
        (rect.top, rect.height())
    };
    let mut cell = el()
        .absolute(rect.left, top)
        .w(rect.width())
        .h(height)
        .cursor(Cursor::Pointer)
        .role("button")
        .label(row.clone())
        .key(format!("row-{}", s.spec.above + index))
        .on_hover_change(move |s: &mut Picker, inside, _| {
            s.hover = inside.then_some(index);
        })
        .press_on_down()
        .on_press(move |s: &mut Picker| s.click(index))
        .child(
            el().absolute(picker::PAD, super::rune::ink_y(height, 14.0))
                .w(rect.width() - 2.0 * picker::PAD)
                .child(
                    text(ellipsize(&row, rect.width() - 2.0 * picker::PAD, 14.0))
                        .font(TextStyle::new(14.0, 400.0))
                        .color(theme::srgb(if selected {
                            palette.selected_text
                        } else {
                            palette.text
                        }))
                        .a11y_hidden(),
                ),
        );
    if selected {
        cell = cell
            .radius(picker::RADIUS)
            .fill(theme::srgb(palette.selected_bg));
    } else if hovered {
        cell = cell
            .radius(picker::RADIUS)
            .fill(theme::srgb_alpha(palette.hover, 0.66));
    }
    cell
}

/// 卷动提示:上下还有内容时在列表右缘画小箭头。
fn scroll_hints_el(s: &Picker, palette: &Palette) -> El {
    let list = s.layout.list;
    let mut hints = el().w_full().h_full().a11y_hidden();
    if s.spec.above > 0 {
        hints = hints.child(
            el().absolute(list.width() - 16.0, 5.0).child(
                text("▲")
                    .font(TextStyle::new(10.0, 400.0))
                    .color(theme::srgb(palette.text_tertiary))
                    .a11y_hidden(),
            ),
        );
    }
    if s.spec.below > 0 {
        hints = hints.child(
            el().absolute(list.width() - 16.0, list.height() - 15.0)
                .child(
                    text("▼")
                        .font(TextStyle::new(10.0, 400.0))
                        .color(theme::srgb(palette.text_tertiary))
                        .a11y_hidden(),
                ),
        );
    }
    hints
}

/// 页脚:操作提示左,当前位置右。
fn footer_el(spec: &picker::Spec, layout: &picker::Layout, palette: &Palette) -> El {
    let pad = picker::PAD;
    let footer = layout.footer;
    let position = if spec.total == 0 {
        String::new()
    } else {
        format!("{}/{}", spec.above + spec.selected + 1, spec.total)
    };
    el().absolute(0.0, footer.top)
        .w(footer.width())
        .h(footer.height())
        .child(
            el().absolute(pad, super::rune::ink_y(footer.height(), 11.0))
                .w(footer.width() - 2.0 * pad)
                .child(
                    text("↑↓ 选择 · Enter 确认 · Esc 取消")
                        .font(TextStyle::new(11.0, 400.0))
                        .color(theme::srgb(palette.text_tertiary))
                        .a11y_hidden(),
                ),
        )
        .child(
            el().absolute(
                footer.width() - pad - 40.0,
                super::rune::ink_y(footer.height(), 11.0),
            )
            .w(40.0)
            .child(
                text(position)
                    .font(TextStyle::new(11.0, 400.0))
                    .color(theme::srgb(palette.text_tertiary))
                    .a11y_hidden(),
            ),
        )
}

/// 文本按可宽截断;行宽以内用省略号。与候选窗同一套估宽截断
/// (`panel::Estimate`),字符宽近似 ASCII 半角、其余全角,不为面板
/// 引入一套文本测量。
fn ellipsize(text: &str, budget: f32, size: f32) -> String {
    panel::ellipsize(text, budget, size, &panel::Estimate)
}

/// 视图转渲染输入。
fn spec_of(view: &HostPanelView) -> picker::Spec {
    picker::Spec {
        title: view.title.clone(),
        rows: view.rows.clone(),
        selected: view.selected,
        total: view.total,
        above: view.above,
        below: view.below,
    }
}

/// 唤出时的前台窗口(裸句柄值);没有就退到光标位置定位。
fn foreground() -> isize {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        0
    } else {
        hwnd.0 as isize
    }
}

/// 窗口矩形;取不到给空矩形兜底(定位退到光标)。
fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        rect = RECT::default();
    }
    rect
}

/// 自检:`lueying-server.exe --panel-selftest <报告>`。独立进程:主线程跑
/// 一个只含面板窗口的 Rune App,助手线程投递两单(方向键选中、Esc 取消)、
/// 注入按键并断言焦点归还;结论写文件供 win-e2e 断言。不碰单实例锁、
/// 管道与 TSF。
pub(crate) fn selftest(report: &std::path::Path) -> Result<(), String> {
    match selftest_run(report) {
        // 成功路径在助手线程里写报告并退出进程,这里不会返回。
        Ok(()) => Ok(()),
        Err(e) => {
            let mut lines = vec!["宿主选择面板自检".to_string()];
            lines.push(format!("结果:失败 — {e}"));
            if let Some(dir) = report.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(report, lines.join("\n"));
            Err(e)
        }
    }
}

/// 自检的一轮:投递一单、等面板拿到前台、注入按键、取选择结果。
fn round(
    tx: &mpsc::SyncSender<PanelJob>,
    panel: HostPanel,
    keys: &[TestKey],
) -> Result<Option<u32>, String> {
    let (done, done_rx) = Done::channel();
    tx.try_send(PanelJob { panel, done })
        .map_err(|_| "投递面板请求失败".to_string())?;
    if let Some(app) = APP.get() {
        app.wake();
    }
    // 等面板拿到前台;进程自己持有宿主的前台权,被拒重试必成。
    wait_picker_foreground(Duration::from_secs(3))?;
    std::thread::sleep(Duration::from_millis(80));
    for key in keys {
        match key {
            TestKey::Enter => send_vk(VK_RETURN),
            TestKey::Esc => send_vk(VK_ESCAPE),
            TestKey::Down => send_vk(VK_DOWN),
        }
    }
    let out = done_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "宿主面板 10 秒内没有收起".to_string())?;
    let out = out.map_err(|e| format!("面板回传错误: {e}"))?;
    let selected = serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|value| value.get("selected")?.as_u64().map(|v| v as u32));
    Ok(selected)
}

fn selftest_run(report: &std::path::Path) -> Result<(), String> {
    let host = create_host_window()?;
    unsafe {
        let _ = ShowWindow(host, SW_SHOW);
    }
    // 新进程第一次抢前台可能被系统拒绝,重试到超时;之后的焦点归还是严格断言。
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { GetForegroundWindow() } != host {
        if Instant::now() >= deadline {
            return Err("自检宿主没有拿到前台".into());
        }
        unsafe {
            let _ = SetForegroundWindow(host);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (tx, rx) = mpsc::sync_channel::<PanelJob>(QUEUE);
    let report = report.to_path_buf();
    // HWND 不跨线程:裸句柄值传给助手线程,用的时候再包回来。
    let host_handle = host.0 as isize;
    // 助手线程:投递、注键、核对结果;写完报告退出进程(事件循环不返回)。
    std::thread::Builder::new()
        .name("panel-selftest".into())
        .spawn(move || {
            let host = HWND(host_handle as *mut _);
            let mut lines: Vec<String> = Vec::new();
            let outcome = (|| -> Result<(), String> {
                let panel = || {
                    HostPanel::parse(r#"{"title":"选择语言","items":["中文","English","日本語"]}"#)
                };
                let first = round(&tx, panel()?, &[TestKey::Down, TestKey::Enter])?;
                if first != Some(1) {
                    return Err(format!("方向键选择回传 {first:?},期望 Some(1)"));
                }
                wait_foreground(host, Duration::from_secs(2))?;
                lines.push("宿主选择面板:方向键 + Enter 选中第二项,焦点已归还".into());
                let second = round(&tx, panel()?, &[TestKey::Esc])?;
                if second.is_some() {
                    return Err(format!("Esc 仍回传 {second:?}"));
                }
                wait_foreground(host, Duration::from_secs(2))?;
                lines.push("宿主选择面板:Esc 取消,焦点已归还".into());
                Ok(())
            })();
            match &outcome {
                Ok(()) => lines.push("结果:通过".to_string()),
                Err(e) => lines.push(format!("结果:失败 — {e}")),
            }
            let _ = std::fs::write(&report, lines.join("\n"));
            std::process::exit(if outcome.is_ok() { 0 } else { 1 });
        })
        .map_err(|e| format!("起面板自检线程: {e}"))?;
    // 主线程:只含面板窗口的 Rune App;面板在第一次投递时打开。
    let mut app = App::new(AppSpec::new(
        "lueying-panel-selftest",
        (100.0, 100.0),
        placeholder,
        empty_view,
    ))
    .windowless();
    let (picker_app, picker_window) = app.window(window_spec(rx));
    attach(picker_app, picker_window);
    app.run();
    Ok(())
}

/// 只含面板窗口的自检宿主没有画面;这个 view 恒空。
fn empty_view(_: &Picker, _: &mut Cx<Picker>) -> El {
    el()
}

/// 等宿主选择面板成为前台(按窗口标题认)。
fn wait_picker_foreground(timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if foreground_title() == "掠影选择面板" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("宿主选择面板没有拿到前台".into())
}

fn foreground_title() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 64];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 等焦点回到指定窗口。
fn wait_foreground(target: HWND, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if unsafe { GetForegroundWindow() } == target {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("焦点没有归还给前台窗口".into())
}

/// 自检用按键。
#[derive(Clone, Copy)]
enum TestKey {
    Enter,
    Esc,
    Down,
}

/// 注入一个命名键。
fn send_vk(vk: VIRTUAL_KEY) {
    let mut down = INPUT {
        r#type: INPUT_KEYBOARD,
        ..Default::default()
    };
    down.Anonymous.ki.wVk = vk;
    let mut up = INPUT {
        r#type: INPUT_KEYBOARD,
        ..Default::default()
    };
    up.Anonymous.ki.wVk = vk;
    up.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
    unsafe {
        SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
    }
}

/// 自检宿主窗口:面板要把焦点还到这里。
fn create_host_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let hinstance = HINSTANCE(hmodule.0);
        static ONCE: std::sync::Once = std::sync::Once::new();
        let mut result: Result<(), String> = Ok(());
        ONCE.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(host_proc),
                hInstance: hinstance,
                lpszClassName: w!("lueying-panel-selftest-host"),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                result = Err("RegisterClassW(自检宿主) 失败".into());
            }
        });
        result?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("lueying-panel-selftest-host"),
            w!("掠影面板自检"),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            360,
            220,
            None,
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW(自检宿主): {e}"))
    }
}

/// 自检宿主的窗口过程;`DefWindowProcW` 是 Rust ABI,不能直接当 `WNDPROC`。
unsafe extern "system" fn host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

//! 设置窗:原生 Win32 窗口 + Skia 光栅绘制。
//!
//! 画面与命中都在渲染核心 `lueying_render::ui`,页面模型在
//! `lueying_server::settings_view`;这里只管窗口、消息与线程。
//!
//! 窗口是普通的不透明窗:Skia 画进一块 32 位位图,再 BitBlt 到客户区。
//! 候选窗走 DirectComposition,徽标走分层窗口,设置窗走 GDI——三条路各有
//! 各的理由,只有「像素怎么进屏幕」这一步是共通的。

use std::sync::{Arc, Mutex};

use accesskit::{NodeId, TreeUpdate};

use crate::accents::ACCENTS;
use crate::blit::Layer;
use crate::config::{self, AppConfig};
use crate::font;
use crate::settings_a11y::{self, A11y, QueuedAction, WM_A11Y};
use crate::settings_actions;
use crate::settings_edit::{self, WM_PHRASES_SAVED};
use crate::settings_view::{Action, Notice, PageColors, Settings, UpdateState, ids};
use crate::theme;
use crate::update::{self, Check, Release};
use lueying_protocol::Request;
use lueying_render::panel::Palette;
use lueying_render::ui::{self, Id};
use skia_safe::Font;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetDC, HDC, InvalidateRect, PAINTSTRUCT, ReleaseDC, UpdateWindow,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem,
    GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SWP_NOZORDER, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MOUSEWHEEL,
    WM_PAINT, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

/// 设置窗的逻辑宽(逻辑像素);渲染核心按这个宽度排版。
const WIDTH: f32 = 460.0;
/// 设置窗的逻辑高;屏幕不够高时按可用高度收缩。
const HEIGHT: f32 = 650.0;
/// 后台查更新回来后窗口收到的消息。
const WM_UPDATE_DONE: u32 = WM_APP + 1;
/// 一格滚轮滚多远(物理像素)。
const WHEEL_STEP: f32 = 48.0;

/// 后台查更新的结果:线程写,窗口线程读。
static UPDATE_RESULT: Mutex<Option<Result<Check, String>>> = Mutex::new(None);

/// 打开设置窗:建窗、跑消息循环,窗口关掉才退出。
pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let path = config::path();
    let cfg = path
        .as_ref()
        .map_or_else(AppConfig::default, |path| config::load(path));
    let mut settings = Settings::with_config(cfg);
    if let Some(issue) = settings.cfg.config_issues.first() {
        let message = format!(
            "配置有 {} 处问题:{} ({})",
            settings.cfg.config_issues.len(),
            issue.message,
            issue.layer
        );
        settings.say(Notice::Error, message);
    }
    settings.path = path;
    settings.groups = settings_actions::call(&Request::FuzzyStats)
        .map(|reply| reply.groups)
        .unwrap_or_default();
    settings.status = settings_actions::call(&Request::Status).and_then(|reply| reply.status);
    settings.current = update::current_version();
    settings.effects_detail = effects_detail();
    settings.scripts = settings_actions::installed_scripts();
    settings.ppocr_ready = settings_actions::ppocr_ready();
    settings.ext_metrics = settings_actions::read_ext_metrics();

    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            eprintln!("设置窗建窗失败: {e}");
            return;
        }
    };
    let mut window = match Window::new(settings) {
        Ok(window) => window,
        Err(e) => {
            eprintln!("设置窗初始化失败: {e}");
            return;
        }
    };
    unsafe {
        window.scale = dpi_of(hwnd);
        window.size = client_size(hwnd);
        window.relayout();
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            Box::into_raw(Box::new(window)) as isize,
        );
        // AccessKit 必须在首次显示前子类化窗口,才能截到 WM_GETOBJECT。
        if let Some(window) = state_of(hwnd) {
            window.enable_a11y(hwnd);
        }
        ShowWindow(hwnd, SW_SHOW);
        // 首次显示前把整块客户区标脏,第一帧就是最新配置。
        let _ = InvalidateRect(Some(hwnd), None, false);
        UpdateWindow(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // 短语编辑器没有对话框管理器:Esc 与 Tab 在这里补。
            if settings_edit::handle_key(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 效果目录的说明文字。
fn effects_detail() -> String {
    match config::effects_dir() {
        Some(dir) => format!("SkSL 装饰层,{}", dir.display()),
        None => "SkSL 装饰层,没有可写的目录".to_string(),
    }
}

/// 自检:建窗、画一帧、贴一次,再读回像素确认画面不是空的。
///
/// Windows CI 跑这条,证明原生设置窗这条链路没断:窗口、光栅、位图、
/// GDI 贴图,每一步都得通。报告写到文件里,CI 收进产物。
pub fn selftest(report: &std::path::Path) -> Result<(), String> {
    let mut lines: Vec<String> = Vec::new();
    let result = selftest_run(&mut lines);
    match &result {
        Ok(()) => lines.push("结果:通过".to_string()),
        Err(e) => lines.push(format!("结果:失败 — {e}")),
    }
    let _ = std::fs::write(report, lines.join("\n"));
    result
}

/// 自检的正文。
fn selftest_run(lines: &mut Vec<String>) -> Result<(), String> {
    let hwnd = create_window()?;
    let settings = Settings::default();
    let mut window = Window::new(settings)?;
    window.scale = dpi_of(hwnd);
    window.size = client_size(hwnd);
    window.relayout();
    let (w, h) = window.size;
    lines.push(format!("客户区 {w}×{h} 像素"));
    if w <= 0 || h <= 0 {
        return Err("客户区没有尺寸".to_string());
    }
    // 画一帧。
    {
        let surface = window
            .layer
            .surface(w as u32, h as u32)
            .map_err(|e| format!("光栅表面: {e}"))?;
        let canvas = surface.canvas();
        ui::Painter::new(canvas, &window.palette, &window.font, window.scale).paint(
            &window.page,
            &window.layout,
            None,
            0.0,
        );
    }
    // 像素搬进位图,再贴一次:走通 GDI 那一步。
    if let Err(e) = window.layer.copy_pixels() {
        return Err(format!("搬像素: {e}"));
    }
    unsafe {
        let hdc = GetDC(Some(hwnd));
        let blit = window.layer.blit(hdc, w, h);
        let _ = ReleaseDC(Some(hwnd), hdc);
        blit.map_err(|e| format!("BitBlt: {e}"))?;
    }
    // 读回像素:角落是窗口底色,卡片里是卡片色。
    let pixels = window.layer.pixels().ok_or("像素读回失败")?;
    let at = |x: i32, y: i32| -> (u8, u8, u8) {
        let index = ((y * w + x) * 4) as usize;
        (pixels[index + 2], pixels[index + 1], pixels[index])
    };
    let corner = at(2, 2);
    let expect_corner = bgr(window.palette.window_bg);
    if corner != expect_corner {
        return Err(format!("窗口底色不对: {corner:?} 期望 {expect_corner:?}"));
    }
    // 卡片是圆角的,角上那一点落在圆角外,是窗口底色;取上边缘中点
    // 才是卡片底色。
    let card_rect = window.layout.card;
    let card_x = (card_rect.left + card_rect.width() / 2.0) as i32;
    let card_y = card_rect.top as i32 + 2;
    let card = at(card_x, card_y);
    let expect_card = bgr(window.palette.bg);
    if card != expect_card {
        return Err(format!("卡片底色不对: {card:?} 期望 {expect_card:?}"));
    }
    lines.push(format!("窗口底色 {corner:?} 卡片底色 {card:?}"));
    // 命中区能对上:第一个开关点得到。
    let toggle = ui::Id::new(ids::AUX, 0);
    let rect = window
        .layout
        .card_hit(toggle)
        .ok_or("找不到辅助码开关的命中区")?;
    let hit = window.layout.hit_card(
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
    if hit != Some(toggle) {
        return Err(format!("开关命中不对: {hit:?}"));
    }
    lines.push("开关命中正常".to_string());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

/// 0xRRGGBB 转成与像素同序的 (R, G, B)。
fn bgr(rgb: u32) -> (u8, u8, u8) {
    ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// 窗口状态:配置、页面、几何与绘制资源。
struct Window {
    settings: Settings,
    appearance: theme::Appearance,
    accent: usize,
    palette: Palette,
    page: ui::Page,
    layout: ui::Layout,
    layer: Layer,
    font: Font,
    /// 卡片往上滚了多远(物理像素)。
    scroll: f32,
    /// 键盘焦点落在哪个控件上。
    focus: Option<Id>,
    /// DPI 缩放。
    scale: f32,
    /// 客户区尺寸(物理像素)。
    size: (i32, i32),
    /// 待安装的新版。
    release: Option<Release>,
    /// 无障碍树快照;适配器激活时每次重排后下发。
    a11y_tree: Arc<Mutex<TreeUpdate>>,
    /// 无障碍节点到控件 ID 的反查表。
    a11y_ids: Vec<(NodeId, Id)>,
    /// 读屏软件动作队列;窗口线程消费。
    a11y_actions: Arc<Mutex<Vec<QueuedAction>>>,
    /// AccessKit 适配器;自检与无读屏时留空。
    a11y: Option<A11y>,
}

impl Window {
    fn new(settings: Settings) -> Result<Self, String> {
        let appearance = theme::Appearance::of_system();
        let accent = settings.cfg.ui_accent.min(ACCENTS.len() - 1);
        let colors = theme::colors_with(appearance, accent);
        let palette = theme::palette(colors);
        let font = font::ui_font();
        let layer = Layer::new()?;
        let scale = system_scale();
        let page = settings.page(&page_colors(&colors, appearance));
        let layout = ui::layout(&page, &font, scale, HEIGHT * scale);
        let snapshot = settings_a11y::page_tree(&page, &layout, &settings, None, 0.0, (0, 0));
        Ok(Self {
            settings,
            appearance,
            accent,
            palette,
            page,
            layout,
            layer,
            font,
            scroll: 0.0,
            focus: None,
            scale,
            size: (0, 0),
            release: None,
            a11y_tree: Arc::new(Mutex::new(snapshot.update)),
            a11y_ids: snapshot.ids,
            a11y_actions: Arc::new(Mutex::new(Vec::new())),
            a11y: None,
        })
    }

    /// 配置变了就重排一页:配色、页面与几何都跟着走。
    fn relayout(&mut self) {
        let colors = theme::colors_with(self.appearance, self.accent);
        self.palette = theme::palette(colors);
        self.page = self.settings.page(&page_colors(&colors, self.appearance));
        self.layout = ui::layout(&self.page, &self.font, self.scale, self.size.1 as f32);
        let max = self.layout.max_scroll();
        self.scroll = self.scroll.clamp(0.0, max);
        self.publish_a11y();
    }

    /// 重建无障碍树并下发给读屏软件。
    fn publish_a11y(&mut self) {
        let snapshot = settings_a11y::page_tree(
            &self.page,
            &self.layout,
            &self.settings,
            self.focus,
            self.scroll,
            self.size,
        );
        self.a11y_ids = snapshot.ids;
        *self
            .a11y_tree
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = snapshot.update;
        if let Some(a11y) = self.a11y.as_mut() {
            a11y.refresh();
        }
    }

    /// 挂上 AccessKit;必须在窗口首次显示前调用。
    fn enable_a11y(&mut self, hwnd: HWND) {
        self.a11y = Some(A11y::new(
            hwnd,
            Arc::clone(&self.a11y_tree),
            Arc::clone(&self.a11y_actions),
        ));
    }

    /// 取出读屏软件排队的动作。
    fn take_a11y_actions(&self) -> Vec<QueuedAction> {
        std::mem::take(
            &mut *self
                .a11y_actions
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    /// 执行一条无障碍动作:点击与鼠标点走同一条路,焦点只移动焦点。
    fn a11y_action(&mut self, queued: QueuedAction) -> Option<Action> {
        match queued {
            QueuedAction::Click(node) => {
                let id = self
                    .a11y_ids
                    .iter()
                    .find_map(|(candidate, id)| (*candidate == node).then_some(*id))?;
                self.press(id)
            }
            QueuedAction::Focus(node) => {
                let id = self
                    .a11y_ids
                    .iter()
                    .find_map(|(candidate, id)| (*candidate == node).then_some(*id))?;
                self.focus = Some(if Settings::is_segment(id.base) {
                    Id::new(id.base, 0)
                } else {
                    id
                });
                self.relayout();
                self.scroll_to_focus();
                self.publish_a11y();
                None
            }
        }
    }

    /// 画一帧:Skia 画进位图,再贴到客户区。
    fn paint(&mut self, hdc: HDC) {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        let (w, h) = (w as u32, h as u32);
        {
            let Ok(surface) = self.layer.surface(w, h) else {
                return;
            };
            let canvas = surface.canvas();
            ui::Painter::new(canvas, &self.palette, &self.font, self.scale).paint(
                &self.page,
                &self.layout,
                self.focus,
                self.scroll,
            );
        }
        let _ = self.layer.copy_pixels();
        let _ = self.layer.blit(hdc, w as i32, h as i32);
    }

    /// 点到哪个控件了;页面坐标(客户区左上角为原点)。
    fn hit(&self, x: f32, y: f32) -> Option<Id> {
        if let Some(id) = self.layout.hit_page(x, y) {
            return Some(id);
        }
        if y < self.layout.scroll.top || y >= self.layout.scroll.bottom {
            return None;
        }
        let card_x = x - self.layout.card.left;
        let card_y = y + self.scroll - self.layout.card.top;
        self.layout.hit_card(card_x, card_y)
    }

    /// 点一下:改配置或者给平台侧一个动作。
    fn click(&mut self, x: f32, y: f32) -> Option<Action> {
        let id = self.hit(x, y)?;
        self.press(id)
    }

    /// 按下控件:读屏软件与键盘激活共用这一条。
    fn press(&mut self, id: Id) -> Option<Action> {
        if let Some(button) = self.page.button(id)
            && !button.enabled
        {
            return None;
        }
        // 点分段格:焦点落到整组上,左右键才好使。
        self.focus = Some(if Settings::is_segment(id.base) {
            Id::new(id.base, 0)
        } else {
            id
        });
        let action = self.settings.activate(&id);
        self.relayout();
        action
    }

    /// Tab 走一圈。
    fn focus_step(&mut self, delta: isize) {
        let order = self.settings.focus_ids();
        if order.is_empty() {
            return;
        }
        let next = match self.focus {
            Some(current) => {
                let index = order.iter().position(|id| *id == current).unwrap_or(0);
                ((index as isize) + delta).rem_euclid(order.len() as isize) as usize
            }
            None => {
                if delta > 0 {
                    0
                } else {
                    order.len() - 1
                }
            }
        };
        self.focus = Some(order[next]);
        self.scroll_to_focus();
        self.publish_a11y();
    }

    /// 焦点控件滚进视野。
    fn scroll_to_focus(&mut self) {
        let Some(id) = self.focus else {
            return;
        };
        // 头部与页脚不在滚动区里,不用管。
        let Some(rect) = self
            .layout
            .card_hit(id)
            .or_else(|| self.layout.ring_card(id))
        else {
            return;
        };
        let card_top = self.layout.card.top;
        let top = card_top + rect.top - self.scroll;
        let bottom = card_top + rect.bottom - self.scroll;
        if top < self.layout.scroll.top {
            self.scroll -= self.layout.scroll.top - top;
        } else if bottom > self.layout.scroll.bottom {
            self.scroll += bottom - self.layout.scroll.bottom;
        }
        let max = self.layout.max_scroll();
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// 左右键改分段组的值。
    fn shift_segment(&mut self, delta: isize) {
        let Some(id) = self.focus else {
            return;
        };
        if !self.settings.shift(&id, delta) {
            return;
        }
        self.relayout();
    }

    /// Enter / Space 激活焦点上的控件。
    fn activate_focus(&mut self) -> Option<Action> {
        let id = self.focus?;
        self.press(id)
    }

    /// 滚轮:向上滚看更早的内容。
    fn wheel(&mut self, delta: f32) {
        let max = self.layout.max_scroll();
        self.scroll = (self.scroll - delta / 120.0 * WHEEL_STEP * self.scale).clamp(0.0, max);
        self.publish_a11y();
    }

    /// 后台查更新回来了。
    fn apply_update(&mut self, result: Result<Check, String>) {
        self.settings.updates = match result {
            Ok(Check::Latest) => UpdateState::Latest,
            Ok(Check::Available(release)) => {
                self.release = Some(release.clone());
                UpdateState::Available(release.version)
            }
            Ok(Check::NoRelease) => UpdateState::NoRelease,
            Err(error) => UpdateState::Failed(error),
        };
        self.relayout();
    }

    /// 按键:Tab 换焦点、左右改分段、Enter 激活、Esc 关闭。
    fn key(&mut self, key: u32, shift: bool, ctrl: bool) -> Option<Action> {
        if ctrl && key == u32::from(b'S') {
            return Some(Action::Save);
        }
        match key {
            k if k == u32::from(VK_TAB.0) => {
                self.focus_step(if shift { -1 } else { 1 });
                None
            }
            k if k == u32::from(VK_LEFT.0) => {
                self.shift_segment(-1);
                None
            }
            k if k == u32::from(VK_RIGHT.0) => {
                self.shift_segment(1);
                None
            }
            k if k == u32::from(VK_RETURN.0) || k == u32::from(VK_SPACE.0) => self.activate_focus(),
            k if k == u32::from(VK_ESCAPE.0) => Some(Action::Close),
            _ => None,
        }
    }
}

/// 页面里要用的颜色:提示色与主题色选项的着色。
fn page_colors(colors: &theme::Colors, appearance: theme::Appearance) -> PageColors {
    let accents = if matches!(appearance, theme::Appearance::HighContrast) {
        vec![colors.accent; ACCENTS.len()]
    } else {
        ACCENTS
            .iter()
            .map(|accent| match appearance {
                theme::Appearance::Dark => accent.dark[0],
                _ => accent.light[0],
            })
            .collect()
    };
    PageColors {
        notice_info: colors.text_secondary,
        notice_success: colors.success,
        notice_error: colors.danger,
        accents,
    }
}

/// 窗口状态挂在窗口的 USERDATA 上。
fn state_of(hwnd: HWND) -> Option<&'static mut Window> {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Window;
        ptr.as_mut()
    }
}

/// 取出设置状态:动作期间不持有窗口的借用,模态对话框才敢重入消息循环。
fn take_settings(hwnd: HWND) -> Option<Settings> {
    state_of(hwnd).map(|window| std::mem::take(&mut window.settings))
}

/// 把设置状态放回窗口并重排。
fn put_settings(hwnd: HWND, settings: Settings) {
    if let Some(window) = state_of(hwnd) {
        window.settings = settings;
        window.relayout();
    }
}

/// 执行动作。有些动作会开模态对话框,那时窗口会重入消息循环。
fn handle_action(hwnd: HWND, action: Action) {
    match action {
        Action::Close => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        Action::Save => with_settings(hwnd, settings_actions::save),
        Action::ResetFuzzy => with_settings(hwnd, settings_actions::reset_fuzzy),
        Action::OpenEffects => with_settings(hwnd, settings_actions::open_effects),
        Action::ChooseScript => {
            with_settings(hwnd, |settings| {
                settings_actions::choose_script(hwnd, settings);
            });
        }
        Action::OpenScripts => with_settings(hwnd, settings_actions::open_scripts),
        Action::ApproveExtension => with_settings(hwnd, settings_actions::approve_extension),
        Action::OpenExtAudit => with_settings(hwnd, settings_actions::open_ext_audit),
        Action::RefreshExtMetrics => with_settings(hwnd, settings_actions::refresh_ext_metrics),
        Action::OpenOcrModels => with_settings(hwnd, settings_actions::open_ocr_models),
        Action::OpenPhrases => open_phrase_editor(hwnd),
        Action::OpenPunct => with_settings(hwnd, settings_actions::open_punct),
        Action::OpenApps => with_settings(hwnd, settings_actions::open_apps),
        Action::BackupExport => with_settings(hwnd, settings_actions::backup_export),
        Action::BackupImport => with_settings(hwnd, settings_actions::backup_import),
        Action::ImportUser => with_settings(hwnd, settings_actions::import_user),
        Action::ExportUser => with_settings(hwnd, settings_actions::export_user),
        Action::Diagnostics => with_settings(hwnd, settings_actions::export_diagnostics),
        Action::ClearUser => {
            let Some(mut settings) = take_settings(hwnd) else {
                return;
            };
            if settings.confirm_clear {
                settings_actions::clear_user(&mut settings);
            } else {
                settings.confirm_clear = true;
                settings.say(Notice::Info, "再点一次确认清空");
            }
            put_settings(hwnd, settings);
        }
        Action::CheckUpdate => start_update_check(hwnd),
        Action::InstallUpdate(_) => {
            let release = state_of(hwnd).and_then(|window| window.release.clone());
            let Some(release) = release else {
                return;
            };
            let Some(mut settings) = take_settings(hwnd) else {
                return;
            };
            if settings_actions::install_update(&mut settings, &release) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            } else {
                put_settings(hwnd, settings);
            }
        }
    }
}

/// 打开窗内短语编辑器:标准 EDIT 控件,读屏软件与输入法都能正常工作。
fn open_phrase_editor(hwnd: HWND) {
    if let Err(error) = settings_edit::open(hwnd) {
        with_settings(hwnd, |settings| {
            settings.say(Notice::Error, error);
        });
    }
}

/// 取出设置、跑动作、放回去。
fn with_settings(hwnd: HWND, f: impl FnOnce(&mut Settings)) {
    let Some(mut settings) = take_settings(hwnd) else {
        return;
    };
    f(&mut settings);
    put_settings(hwnd, settings);
}

/// 后台查更新:网络慢不能卡住窗口。
fn start_update_check(hwnd: HWND) {
    let channel = state_of(hwnd)
        .map(|window| window.settings.cfg.update_channel)
        .unwrap_or_default();
    if let Some(window) = state_of(hwnd) {
        window.settings.updates = UpdateState::Checking;
        window.relayout();
    }
    *UPDATE_RESULT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    // 句柄不能跨线程传(HWND 里面是裸指针),只把地址带过去,线程里再装回去。
    let hwnd_raw = hwnd.0 as usize;
    let spawned = std::thread::Builder::new()
        .name("settings-update".into())
        .spawn(move || {
            let result = update::check_channel(channel);
            *UPDATE_RESULT.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
            unsafe {
                let hwnd = HWND(hwnd_raw as *mut std::ffi::c_void);
                let _ = PostMessageW(Some(hwnd), WM_UPDATE_DONE, WPARAM(0), LPARAM(0));
            }
        });
    if let Err(e) = spawned {
        eprintln!("检查更新的线程启动失败: {e}");
    }
}

/// 客户区尺寸(物理像素)。
fn client_size(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_ok() {
            (rect.right - rect.left, rect.bottom - rect.top)
        } else {
            (0, 0)
        }
    }
}

/// 窗口的 DPI 缩放。
fn dpi_of(hwnd: HWND) -> f32 {
    unsafe { GetDpiForWindow(hwnd).max(96) as f32 / 96.0 }
}

/// 系统的 DPI 缩放。
fn system_scale() -> f32 {
    unsafe { GetDpiForSystem().max(96) as f32 / 96.0 }
}

/// lparam 里的低 16 位(鼠标坐标)。
fn low_word(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xffff) as i16 as i32
}

/// lparam 里的高 16 位(鼠标坐标)。
fn high_word(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) & 0xffff) as i16 as i32
}

/// 窗口过程。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut paint);
                if let Some(window) = state_of(hwnd) {
                    window.paint(hdc);
                }
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                if let Some(window) = state_of(hwnd) {
                    window.size = client_size(hwnd);
                    window.relayout();
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // 系统主题变了就换配色。
                if let Some(window) = state_of(hwnd) {
                    window.appearance = theme::Appearance::of_system();
                    window.relayout();
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let scale = dpi_of(hwnd);
                if let Some(window) = state_of(hwnd) {
                    window.scale = scale;
                    window.relayout();
                }
                // 系统建议的新位置与大小。
                let rect = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER,
                );
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = low_word(lparam) as f32;
                let y = high_word(lparam) as f32;
                let action = state_of(hwnd).and_then(|window| window.click(x, y));
                if let Some(action) = action {
                    handle_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                if let Some(window) = state_of(hwnd) {
                    window.wheel(delta as f32);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u32;
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let action = state_of(hwnd).and_then(|window| window.key(key, shift, ctrl));
                if let Some(action) = action {
                    handle_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                // 读屏软件要按实际焦点决定读哪里;焦点变化后重新下发树。
                if let Some(window) = state_of(hwnd) {
                    window.publish_a11y();
                }
                LRESULT(0)
            }
            WM_A11Y => {
                let actions = state_of(hwnd)
                    .map(|window| window.take_a11y_actions())
                    .unwrap_or_default();
                for queued in actions {
                    let action = state_of(hwnd).and_then(|window| window.a11y_action(queued));
                    if let Some(action) = action {
                        handle_action(hwnd, action);
                    }
                }
                LRESULT(0)
            }
            WM_PHRASES_SAVED => {
                if let Some(window) = state_of(hwnd) {
                    window.settings.say(Notice::Success, "短语已保存");
                    window.relayout();
                }
                LRESULT(0)
            }
            WM_UPDATE_DONE => {
                let result = UPDATE_RESULT
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take();
                if let Some(result) = result
                    && let Some(window) = state_of(hwnd)
                {
                    window.apply_update(result);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    drop(Box::from_raw(ptr as *mut Window));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// 建窗:普通不透明窗,客户区正好是 460×650 逻辑像素。
fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let dpi = GetDpiForSystem().max(96);
        let scale = dpi as f32 / 96.0;
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let height = (HEIGHT * scale).min((screen_h as f32 - 24.0 * scale).max(480.0 * scale));
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: (WIDTH * scale) as i32,
            bottom: height as i32,
        };
        let _ = AdjustWindowRectExForDpi(
            &mut rect,
            WS_OVERLAPPEDWINDOW,
            false,
            WINDOW_EX_STYLE(0),
            dpi,
        );
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let x = ((screen_w - width) / 2).max(0);
        let y = ((screen_h - height) / 2).max(0);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("lueying-settings"),
            w!("掠影设置"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(HINSTANCE(hmodule.0)),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        Ok(hwnd)
    }
}

/// 窗口类进程内只注册一次。
fn ensure_class(hinstance: HINSTANCE) -> Result<(), String> {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: w!("lueying-settings"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW 失败".into());
        }
    });
    result
}

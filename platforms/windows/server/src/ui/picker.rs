//! 宿主选择面板窗口:可获焦的原生窗 + Direct2D 绘制 + GDI 贴图。
//!
//! 扩展 `panel.show({title, items})` 经 PanelProvider 投递到这里;界面线程
//! 一次只显示一单,键盘/鼠标选择后把下标经完成队列回传,取消回传
//! `null`。关闭时把焦点还给唤出前的窗口。

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use lueying_ext::host::Done;
use lueying_render::panel::Palette;
use lueying_render::picker;
use lueying_server::host_panel::{HostPanel, HostPanelView};
use lueying_server::services::PanelJob;
use lueying_server::theme;
use lueying_server::ui_surface::Surface;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::Ime::{HIMC, ImmAssociateContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, SendInput, SetFocus, VIRTUAL_KEY, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClassNameW, GetCursorPos,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, IDC_ARROW, IsWindow,
    LoadCursorW, PostQuitMessage, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use crate::imp::{State, Wake};
use crate::ui::place;

/// 窗口类名;自检按它确认面板已拿到前台。
const CLASS: windows::core::PCWSTR = w!("lueying-host-panel");
/// 请求通道容量:面板开着时再来的请求直接回「正忙」。
const QUEUE: usize = 1;

/// 宿主选择面板:一次显示一单,窗口在第一次请求时才建。请求经通道进来,
/// 显示期间不阻塞界面线程;收起后在循环里把选择回传给请求方。
pub(super) struct Panel {
    jobs: Receiver<PanelJob>,
    window: Option<Box<HostWindow>>,
    /// 正在显示的那一单的完成句柄;收起后用它回传选择。
    current: Option<Done>,
}

/// 装上请求通道:扩展 `panel.show` 投递后经 `wake` 叫醒界面线程。
pub(super) fn init(state: &State, wake: Wake) -> Panel {
    let (tx, rx) = mpsc::sync_channel::<PanelJob>(QUEUE);
    state.panel_service.install(tx, move || wake.set());
    Panel {
        jobs: rx,
        window: None,
        current: None,
    }
}

impl Panel {
    /// 面板开着就等它收起;收起了就回传选择,接着处理排队的下一单。
    pub(super) fn tick(&mut self, state: &State) {
        if self.current.is_some() {
            if !self.window.as_ref().is_some_and(|window| window.hidden) {
                return;
            }
            let selected = self
                .window
                .as_mut()
                .and_then(|window| window.selected.take());
            if let Some(done) = self.current.take() {
                done.complete(Ok(serde_json::json!({ "selected": selected }).to_string()));
            }
        }
        while let Ok(job) = self.jobs.try_recv() {
            if self.start(state, job) {
                return;
            }
        }
    }

    /// 显示一单;建窗或显示失败时直接回传错误,返回 `false` 接着处理下一单。
    fn start(&mut self, state: &State, job: PanelJob) -> bool {
        let PanelJob { panel, done } = job;
        if self.window.is_none() {
            let accent = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .ui_accent;
            match HostWindow::create(accent) {
                Ok(created) => self.window = Some(created),
                Err(error) => {
                    println!("宿主选择面板建窗失败: {error}");
                    done.complete(Err(error));
                    return false;
                }
            }
        }
        let Some(window) = self.window.as_mut() else {
            done.complete(Err("宿主选择面板不可用".into()));
            return false;
        };
        let restore = unsafe { GetForegroundWindow() };
        if let Err(error) = window.show(panel, restore) {
            println!("宿主选择面板显示失败: {error}");
            done.complete(Err(error));
            return false;
        }
        self.current = Some(done);
        true
    }
}

/// 面板窗口状态;由窗口的 USERDATA 指向,生命周期与界面线程一致。
struct HostWindow {
    hwnd: HWND,
    surface: Surface,
    palette: Palette,
    accent: usize,
    scale: f32,
    size: (i32, i32),
    panel: Option<HostPanel>,
    spec: picker::Spec,
    layout: picker::Layout,
    /// 唤出前的前台窗口;关闭时把焦点还给它。
    restore: Option<HWND>,
    visible: bool,
    hidden: bool,
    /// 选中的条目下标;由内层消息循环取走。
    selected: Option<u32>,
}

impl HostWindow {
    /// 建一个隐藏的面板窗口;内容在每次请求时换。
    fn create(accent: usize) -> Result<Box<Self>, String> {
        let mut window = Box::new(Self {
            hwnd: HWND::default(),
            surface: Surface::new()?,
            palette: theme::palette(theme::colors_with(theme::Appearance::of_system(), accent)),
            accent,
            scale: 1.0,
            size: (0, 0),
            panel: None,
            spec: picker::Spec::default(),
            layout: picker::Layout::default(),
            restore: None,
            visible: false,
            hidden: true,
            selected: None,
        });
        let hwnd = create_window()?;
        window.hwnd = hwnd;
        window.scale = dpi_of(hwnd);
        // 面板自己不输入文字,不要让系统输入法在这里起组合。
        unsafe {
            ImmAssociateContext(hwnd, HIMC::default());
        }
        window.reflow();
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *window as *mut Self) as isize);
        }
        Ok(window)
    }

    /// 显示面板;`restore` 是唤出前的前台窗口,关闭时归还焦点。
    fn show(&mut self, panel: HostPanel, restore: HWND) -> Result<(), String> {
        self.restore = (!restore.is_invalid() && restore != self.hwnd).then_some(restore);
        self.panel = Some(panel);
        self.selected = None;
        self.visible = true;
        self.hidden = false;
        self.palette = theme::palette(theme::colors_with(
            theme::Appearance::of_system(),
            self.accent,
        ));
        self.reflow();
        let (w, h) = self.size;
        let (x, y) = self.position();
        unsafe {
            SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW)
                .map_err(|e| format!("SetWindowPos: {e}"))?;
            let _ = SetForegroundWindow(self.hwnd);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        Ok(())
    }

    /// 按当前模型重算视图、行几何与窗口尺寸。
    fn reflow(&mut self) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let view = panel.view(picker::MAX_ROWS);
        self.spec = spec_of(&view);
        self.layout = picker::layout(&self.spec, self.scale);
        let (w, h) = self.layout.size;
        let (w, h) = (w.round() as i32, h.round() as i32);
        if (w, h) != self.size {
            self.size = (w, h);
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    w,
                    h,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// 选中变化后重排并重绘;保持焦点,窗口跟着新尺寸重新居中。
    fn changed(&mut self) {
        self.reflow();
        let (w, h) = self.size;
        let (x, y) = self.position();
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// 面板在还原窗口所在显示器的上半部居中。
    fn position(&self) -> (i32, i32) {
        let (w, h) = self.size;
        let anchor = self
            .restore
            .and_then(|hwnd| {
                let mut rect = RECT::default();
                unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
                Some(((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2))
            })
            .or_else(|| {
                let mut point = POINT::default();
                unsafe { GetCursorPos(&mut point) }.ok()?;
                Some((point.x, point.y))
            });
        let work = anchor
            .and_then(|(x, y)| place::monitor_info_at(x, y))
            .map_or_else(place::primary_work_area, |info| info.rcWork);
        let width = work.right - work.left;
        let height = work.bottom - work.top;
        (
            work.left + ((width - w) / 2).max(0),
            work.top + ((height - h) / 4).max(0),
        )
    }

    /// 画一帧:Direct2D 画进位图,再贴到客户区。
    fn paint(&mut self, hdc: HDC) {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        let Ok(canvas) = self.surface.begin(w as u32, h as u32) else {
            return;
        };
        picker::draw(canvas, &self.spec, &self.layout, &self.palette, self.scale);
        let _ = self.surface.blit(hdc, w, h);
    }

    /// 关闭面板;`restore` 为真时把焦点还给唤出前的窗口。
    fn hide(&mut self, hwnd: HWND, restore: bool) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.hidden = true;
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        if !restore {
            self.restore = None;
            return;
        }
        let Some(target) = self.restore.take() else {
            return;
        };
        unsafe {
            if IsWindow(Some(target)).as_bool() {
                let _ = SetForegroundWindow(target);
            }
        }
    }

    /// 选中当前行并收起。
    fn choose_selected(&mut self, hwnd: HWND) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        self.selected = Some(panel.selected_index() as u32);
        self.hide(hwnd, true);
    }

    /// 点到某行就选中那一行并收起。
    fn click(&mut self, hwnd: HWND, x: f32, y: f32) {
        let picker::Hit::Row(index) = picker::hit(&self.layout, &self.spec, x, y) else {
            return;
        };
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let delta = index as isize - self.spec.selected as isize;
        panel.move_by(delta);
        self.choose_selected(hwnd);
    }

    /// 方向键/Tab 移动选中行;视图卷动跟着走。
    fn move_selection(&mut self, delta: isize) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        if panel.move_by(delta) {
            self.changed();
        }
    }

    /// 窗口消息;返回 `LRESULT`。
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
                self.paint(hdc);
                let _ = unsafe { EndPaint(hwnd, &paint) };
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_KEYDOWN => {
                let key = wparam.0 as u16;
                match key {
                    k if k == VK_UP.0 => self.move_selection(-1),
                    k if k == VK_DOWN.0 => self.move_selection(1),
                    k if k == VK_PRIOR.0 => self.move_selection(-(picker::MAX_ROWS as isize)),
                    k if k == VK_NEXT.0 => self.move_selection(picker::MAX_ROWS as isize),
                    k if k == VK_HOME.0 => self.move_selection(isize::MIN / 2),
                    k if k == VK_END.0 => self.move_selection(isize::MAX / 2),
                    k if k == VK_RETURN.0 => self.choose_selected(hwnd),
                    k if k == VK_ESCAPE.0 => self.hide(hwnd, true),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = low_word(lparam) as f32;
                let y = high_word(lparam) as f32;
                self.click(hwnd, x, y);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                self.move_selection(-(delta / 120 * 3) as isize);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE && self.visible {
                    self.hide(hwnd, false);
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                self.scale = dpi_of(hwnd);
                self.changed();
                LRESULT(0)
            }
            WM_CLOSE => {
                self.hide(hwnd, true);
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }
}

/// 自检专用:显示期间排干消息直到面板收起,取回选中的下标。
fn pump_until_hidden(hwnd: HWND) -> Option<u32> {
    while !state_of(hwnd).is_some_and(|window| window.hidden) {
        crate::ui::wait(None, Some(Duration::from_millis(10)));
        crate::ui::pump_messages();
    }
    state_of(hwnd).and_then(|window| window.selected.take())
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

/// 窗口的 USERDATA 指向面板状态;未挂上时返回 `None`。
fn state_of(hwnd: HWND) -> Option<&'static mut HostWindow> {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostWindow;
        ptr.as_mut()
    }
}

/// 建隐藏窗口;类进程内只注册一次。
fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let hinstance = HINSTANCE(hmodule.0);
        ensure_class(hinstance)?;
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS,
            w!("掠影选择面板"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))
    }
}

fn ensure_class(hinstance: HINSTANCE) -> Result<(), String> {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| unsafe {
        let cursor = match LoadCursorW(None, IDC_ARROW) {
            Ok(cursor) => cursor,
            Err(e) => {
                result = Err(format!("LoadCursorW: {e}"));
                return;
            }
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            hCursor: cursor,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW(宿主选择面板) 失败".into());
        }
    });
    result
}

/// 窗口的 DPI 缩放。
fn dpi_of(hwnd: HWND) -> f32 {
    unsafe { GetDpiForWindow(hwnd).max(96) as f32 / 96.0 }
}

fn low_word(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xffff) as i16 as i32
}

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
        match state_of(hwnd) {
            Some(window) => window.handle(hwnd, msg, wparam, lparam),
            None => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// 自检:方向键 + Enter 选中第二项,Esc 取消,焦点归还。
fn picker_checks(host: HWND, lines: &mut Vec<String>) -> Result<(), String> {
    let panel = HostPanel::parse(r#"{"title":"选择语言","items":["中文","English","日本語"]}"#)?;
    let first = round(panel.clone(), &[Key::Down, Key::Enter])?;
    if first != Some(1) {
        return Err(format!("方向键选择回传 {first:?},期望 Some(1)"));
    }
    wait_foreground(host, Duration::from_secs(2))?;
    lines.push("宿主选择面板:方向键 + Enter 选中第二项,焦点已归还".into());

    let second = round(panel, &[Key::Esc])?;
    if second.is_some() {
        return Err(format!("Esc 仍回传 {second:?}"));
    }
    wait_foreground(host, Duration::from_secs(2))?;
    lines.push("宿主选择面板:Esc 取消,焦点已归还".into());
    Ok(())
}

/// 自检的一轮:起窗口线程、等它拿到前台、注入按键、取选择结果。
fn round(panel: HostPanel, keys: &[Key]) -> Result<Option<u32>, String> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (done_tx, done_rx) = mpsc::channel::<Option<u32>>();
    let spawned = std::thread::Builder::new()
        .name("host-panel-selftest".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let outcome = (|| -> Result<Option<u32>, String> {
                let mut window = HostWindow::create(0)?;
                let restore = unsafe { GetForegroundWindow() };
                window.show(panel, restore)?;
                let _ = ready_tx.send(Ok(()));
                let selected = pump_until_hidden(window.hwnd);
                unsafe {
                    let _ = DestroyWindow(window.hwnd);
                }
                Ok(selected)
            })();
            match outcome {
                Ok(selected) => {
                    let _ = done_tx.send(selected);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    let _ = done_tx.send(None);
                }
            }
        })
        .map_err(|e| format!("起宿主面板自检线程: {e}"))?;
    let ready = ready_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "宿主面板线程 10 秒内没有就绪".to_string())?;
    ready?;
    wait_panel_foreground(Duration::from_secs(3))?;
    std::thread::sleep(Duration::from_millis(80));
    for key in keys {
        match key {
            Key::Enter => send_vk(VK_RETURN),
            Key::Esc => send_vk(VK_ESCAPE),
            Key::Down => send_vk(VK_DOWN),
        }
    }
    let selected = done_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "宿主面板 10 秒内没有收起".to_string())?;
    let _ = spawned.join();
    Ok(selected)
}

/// 等宿主选择面板成为前台。
fn wait_panel_foreground(timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if foreground_class() == "lueying-host-panel" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("宿主选择面板没有拿到前台".into())
}

fn foreground_class() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 自检入口:`lueying-server.exe --panel-selftest <报告>`。独立进程建窗、注入按键,
/// 结论写文件供 win-e2e 断言;不碰单实例锁、管道与 TSF。
pub(crate) fn selftest(report: &std::path::Path) -> Result<(), String> {
    let mut lines = vec!["宿主选择面板自检".to_string()];
    let result = selftest_run(&mut lines);
    match &result {
        Ok(()) => lines.push("结果:通过".to_string()),
        Err(e) => lines.push(format!("结果:失败 — {e}")),
    }
    if let Some(dir) = report.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(report, lines.join("\n"));
    result
}

fn selftest_run(lines: &mut Vec<String>) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
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
            let _ = SetFocus(Some(host));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = picker_checks(host, lines);
    unsafe {
        let _ = DestroyWindow(host);
    }
    result
}

/// 自检用按键。
#[derive(Clone, Copy)]
enum Key {
    Enter,
    Esc,
    Down,
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

/// 自检宿主的窗口过程;`DefWindowProcW` 是 Rust ABI,不能直接当 `WNDPROC`。
unsafe extern "system" fn host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
            windows::Win32::UI::WindowsAndMessaging::WS_OVERLAPPEDWINDOW,
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

/// 注入一个命名键。
fn send_vk(vk: VIRTUAL_KEY) {
    let mut down = INPUT::default();
    down.r#type = INPUT_KEYBOARD;
    down.Anonymous.ki.wVk = vk;
    let mut up = INPUT::default();
    up.r#type = INPUT_KEYBOARD;
    up.Anonymous.ki.wVk = vk;
    up.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
    unsafe {
        SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
    }
}

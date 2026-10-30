//! 命令面板窗口:可获焦的原生窗 + Skia 光栅 + GDI 贴图。
//!
//! 与候选窗不同,面板需要键盘焦点:总热键唤出时记录前台窗口,面板关闭后
//! 把焦点还回去。窗口线程只在有请求时醒来,空闲时阻塞在通道上,不轮询。
//!
//! 画面在 `lueying_render::command`;这里只管窗口、消息、焦点与命令投递。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use lueying_dict::Dictionary;
use lueying_render::command;
use lueying_render::panel::Palette;
use lueying_server::blit::Layer;
use lueying_server::command_panel::{CommandItem, CommandPanel, CommandView};
use lueying_server::font;
use lueying_server::theme;
use skia_safe::Font;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::Ime::{HIMC, ImmAssociateContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, SetFocus,
    VIRTUAL_KEY, VK_BACK, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN,
    VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClassNameW,
    GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST,
    IDC_ARROW, IsWindow, LoadCursorW, MSG, PostQuitMessage, RegisterClassW, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WA_INACTIVE, WINDOW_EX_STYLE,
    WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use crate::imp::{ExtInvoke, State};
use crate::ui_place;

/// 窗口类名;自检按它确认面板已拿到前台。
const CLASS: windows::core::PCWSTR = w!("lueying-command-panel");
/// 命令面板线程的通道容量:面板开着时再按热键直接丢弃,不排队。
const QUEUE: usize = 1;

/// 起面板线程;窗口在第一次唤出时创建,空闲线程阻塞在通道上。
pub(super) fn spawn(state: Arc<State>) {
    let (tx, rx) = mpsc::sync_channel::<Vec<CommandItem>>(QUEUE);
    *state.panel.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
    let spawned = std::thread::Builder::new()
        .name("command-panel".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| panel_loop(state, rx)))
                .is_err()
            {
                println!("命令面板线程崩溃,总热键不再响应");
            }
        });
    if let Err(e) = spawned {
        println!("命令面板线程启动失败: {e}");
    }
}

/// 面板线程:等请求、显示、跑内层消息循环,关掉后把命令投给脚本线程。
fn panel_loop(state: Arc<State>, rx: Receiver<Vec<CommandItem>>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let mut window: Option<Box<PanelWindow>> = None;
    while let Ok(items) = rx.recv() {
        if window.is_none() {
            let accent = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .ui_accent;
            match PanelWindow::create(Arc::clone(&state.dict), accent) {
                Ok(created) => window = Some(created),
                Err(e) => {
                    println!("命令面板建窗失败: {e}");
                    continue;
                }
            }
        }
        let Some(panel) = window.as_mut() else {
            continue;
        };
        let restore = unsafe { GetForegroundWindow() };
        if let Err(e) = panel.show(items, restore) {
            println!("命令面板显示失败: {e}");
            continue;
        }
        state.panel_open.store(true, Ordering::Relaxed);
        let hwnd = panel.hwnd;
        let executed = pump_until_hidden(hwnd);
        state.panel_open.store(false, Ordering::Relaxed);
        if let Some(command) = executed {
            let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
            match hub.as_ref() {
                Some(hub) => {
                    let _ = hub.invoke.send(ExtInvoke {
                        command,
                        source: "命令面板",
                    });
                }
                None => println!("命令面板选中 {command},但扩展线程未就绪"),
            }
        }
    }
}

/// 面板窗口状态;由窗口的 USERDATA 指向,生命周期与窗口线程一致。
struct PanelWindow {
    hwnd: HWND,
    layer: Layer,
    font: Font,
    palette: Palette,
    accent: usize,
    scale: f32,
    size: (i32, i32),
    model: CommandPanel,
    spec: command::Spec,
    layout: command::Layout,
    /// 唤出前的前台窗口;关闭时把焦点还给它。
    restore: Option<HWND>,
    visible: bool,
    hidden: bool,
    /// 选中的命令 id;由内层消息循环取走。
    executed: Option<String>,
}

impl PanelWindow {
    /// 建一个隐藏的面板窗口;命令表在每次唤出时换。
    fn create(dict: Arc<Dictionary>, accent: usize) -> Result<Box<Self>, String> {
        let mut window = Box::new(Self {
            hwnd: HWND::default(),
            layer: Layer::new()?,
            font: font::ui_font(),
            palette: theme::palette(theme::colors_with(theme::Appearance::of_system(), accent)),
            accent,
            scale: 1.0,
            size: (0, 0),
            model: CommandPanel::new(dict, Vec::new()),
            spec: command::Spec::default(),
            layout: command::Layout::default(),
            restore: None,
            visible: false,
            hidden: true,
            executed: None,
        });
        let hwnd = create_window()?;
        window.hwnd = hwnd;
        window.scale = dpi_of(hwnd);
        // 面板自带输入框,不要让系统输入法在这里起组合。
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
    fn show(&mut self, items: Vec<CommandItem>, restore: HWND) -> Result<(), String> {
        self.restore = (!restore.is_invalid() && restore != self.hwnd).then_some(restore);
        self.model.set_items(items);
        self.model.reset();
        self.palette = theme::palette(theme::colors_with(
            theme::Appearance::of_system(),
            self.accent,
        ));
        self.visible = true;
        self.hidden = false;
        self.executed = None;
        self.reflow();
        let (w, h) = self.size;
        let (x, y) = self.position();
        unsafe {
            SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW)
                .map_err(|e| format!("SetWindowPos: {e}"))?;
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        Ok(())
    }

    /// 按当前模型重算视图、行几何与窗口尺寸。
    fn reflow(&mut self) {
        let view: CommandView = self.model.view(command::MAX_ROWS);
        self.spec = spec_of(&view);
        self.layout = command::layout(&self.spec, self.scale);
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

    /// 查询或选中变化后重排并重绘;保持焦点,窗口跟着新尺寸重新居中。
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
            .and_then(|(x, y)| ui_place::monitor_info_at(x, y))
            .map_or_else(ui_place::primary_work_area, |info| info.rcWork);
        let width = work.right - work.left;
        let height = work.bottom - work.top;
        (
            work.left + ((width - w) / 2).max(0),
            work.top + ((height - h) / 4).max(0),
        )
    }

    /// 画一帧:Skia 画进位图,再贴到客户区。
    fn paint(&mut self, hdc: HDC) {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        let Ok(surface) = self.layer.surface(w as u32, h as u32) else {
            return;
        };
        command::draw(
            surface.canvas(),
            &self.spec,
            &self.layout,
            &self.palette,
            &self.font,
            self.scale,
        );
        let _ = self.layer.copy_pixels();
        let _ = self.layer.blit(hdc, w, h);
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

    /// 执行当前选中行。
    fn run_selected(&mut self, hwnd: HWND) {
        let Some(item) = self.model.selected_item() else {
            return;
        };
        let command = item.id.clone();
        self.executed = Some(command);
        self.hide(hwnd, true);
    }

    /// 点到某行就执行那一行;输入框与页脚不执行。
    fn click(&mut self, hwnd: HWND, x: f32, y: f32) {
        let command::Hit::Row(index) = command::hit(&self.layout, &self.spec, x, y) else {
            return;
        };
        let delta = index as isize - self.spec.selected as isize;
        self.model.move_by(delta);
        self.run_selected(hwnd);
    }

    /// 方向键/Tab 移动选中行;视图卷动跟着走。
    fn move_selection(&mut self, delta: isize) {
        if self.model.move_by(delta) {
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
            WM_CHAR => {
                match wparam.0 as u32 {
                    8 => {
                        if self.model.backspace() {
                            self.changed();
                        }
                    }
                    // Enter/Tab/Esc 在 WM_KEYDOWN 里处理;这里只吞掉字符消息。
                    9 | 13 | 27 => {}
                    code => {
                        if let Some(ch) = char::from_u32(code)
                            && !ch.is_control()
                            && self.model.type_char(ch)
                        {
                            self.changed();
                        }
                    }
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u16;
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                match key {
                    k if k == VK_UP.0 => self.move_selection(-1),
                    k if k == VK_DOWN.0 => self.move_selection(1),
                    k if k == VK_PRIOR.0 => self.move_selection(-(command::MAX_ROWS as isize)),
                    k if k == VK_NEXT.0 => self.move_selection(command::MAX_ROWS as isize),
                    k if k == VK_HOME.0 => self.move_selection(isize::MIN / 2),
                    k if k == VK_END.0 => self.move_selection(isize::MAX / 2),
                    k if k == VK_TAB.0 => self.move_selection(if shift { -1 } else { 1 }),
                    k if k == VK_RETURN.0 => self.run_selected(hwnd),
                    k if k == VK_ESCAPE.0 => self.hide(hwnd, true),
                    // 退格由 WM_CHAR 处理;这里吞掉,避免系统响铃。
                    k if k == VK_BACK.0 => {}
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
                // 滚轮向前(正值)看更早的条目。
                self.move_selection(-(delta / 120 * 3) as isize);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                // 点到别的窗口:收起面板,不抢回焦点。
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

/// 内层消息循环:面板显示期间阻塞在这里,收起后返回选中的命令。
fn pump_until_hidden(hwnd: HWND) -> Option<String> {
    loop {
        let mut msg = MSG::default();
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        // 0 = WM_QUIT,-1 = 错误;两种都收工。
        if got.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if state_of(hwnd).is_some_and(|window| window.hidden) {
            break;
        }
    }
    state_of(hwnd).and_then(|window| window.executed.take())
}

/// 视图转渲染输入。
fn spec_of(view: &CommandView) -> command::Spec {
    command::Spec {
        query: view.query.clone(),
        rows: view
            .rows
            .iter()
            .map(|row| command::Row {
                title: row.title.clone(),
                extension: row.extension.clone(),
            })
            .collect(),
        selected: view.selected,
        total: view.total,
        above: view.above,
        below: view.below,
    }
}

/// 窗口的 USERDATA 指向面板状态;未挂上时返回 `None`。
fn state_of(hwnd: HWND) -> Option<&'static mut PanelWindow> {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PanelWindow;
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
            w!("掠影命令面板"),
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
            result = Err("RegisterClassW 失败".into());
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

/// 自检:建窗、模拟热键唤出、拼音搜索、Enter/Esc 与焦点归还。
///
/// 独立进程跑(`lueying-server.exe --panel-selftest <报告>`),不碰管道、
/// 单实例锁与 TSF;CI 由 win-e2e 拉起并读报告。
pub(super) fn selftest(report: &Path) -> Result<(), String> {
    let mut lines = vec!["命令面板自检".to_string()];
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
    // 新进程第一次抢前台可能被系统拒绝,重试到超时;后面每轮的焦点归还是严格断言。
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
    let dict = Arc::new(Dictionary::builtin());
    let items = vec![
        CommandItem {
            id: "a.ocr".into(),
            title: "OCR 取字".into(),
            extension: "demo".into(),
        },
        CommandItem {
            id: "b.xuexi".into(),
            title: "学习".into(),
            extension: "demo".into(),
        },
        CommandItem {
            id: "c.gongzuo".into(),
            title: "工作".into(),
            extension: "demo".into(),
        },
        CommandItem {
            id: "d.dazi".into(),
            title: "打字".into(),
            extension: "demo".into(),
        },
    ];

    // 第一轮:拼音搜索「xuexi」+ Enter,执行第二条命令并归还焦点。
    let first = round(&dict, items.clone(), Some("xuexi"), &[Key::Enter])?;
    if first.as_deref() != Some("b.xuexi") {
        return Err(format!("拼音搜索执行了 {first:?},期望 b.xuexi"));
    }
    wait_foreground(host, Duration::from_secs(2))?;
    lines.push("拼音搜索 + Enter 执行 b.xuexi,焦点已归还".into());

    // 第二轮:空查询下移一格 + Enter,执行第二条。
    let second = round(&dict, items.clone(), None, &[Key::Down, Key::Enter])?;
    if second.as_deref() != Some("b.xuexi") {
        return Err(format!("方向键选择执行了 {second:?},期望 b.xuexi"));
    }
    wait_foreground(host, Duration::from_secs(2))?;
    lines.push("方向键选择 + Enter 执行 b.xuexi,焦点已归还".into());

    // 第三轮:拼音搜索「gongzuo」+ Esc,不执行任何命令。
    let third = round(&dict, items, Some("gongzuo"), &[Key::Esc])?;
    if third.is_some() {
        return Err(format!("Esc 仍执行了 {third:?}"));
    }
    wait_foreground(host, Duration::from_secs(2))?;
    lines.push("拼音搜索 + Esc 取消,焦点已归还".into());

    unsafe {
        let _ = DestroyWindow(host);
    }
    Ok(())
}

/// 自检的一轮:起面板线程、等它拿到前台、注入按键、取执行结果。
fn round(
    dict: &Arc<Dictionary>,
    items: Vec<CommandItem>,
    text: Option<&str>,
    keys: &[Key],
) -> Result<Option<String>, String> {
    let dict = Arc::clone(dict);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (done_tx, done_rx) = mpsc::channel::<Option<String>>();
    let spawned = std::thread::Builder::new()
        .name("panel-selftest".into())
        .spawn(move || {
            // 字型走 DirectWrite,线程上要先有 COM 套间。
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let outcome = (|| -> Result<Option<String>, String> {
                let mut window = PanelWindow::create(dict, 0)?;
                let restore = unsafe { GetForegroundWindow() };
                window.show(items, restore)?;
                let _ = ready_tx.send(Ok(()));
                let executed = pump_until_hidden(window.hwnd);
                unsafe {
                    let _ = DestroyWindow(window.hwnd);
                }
                Ok(executed)
            })();
            match outcome {
                Ok(executed) => {
                    let _ = done_tx.send(executed);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    let _ = done_tx.send(None);
                }
            }
        })
        .map_err(|e| format!("起自检线程: {e}"))?;
    let ready = ready_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "面板线程 10 秒内没有就绪".to_string())?;
    ready?;
    wait_panel_foreground(Duration::from_secs(3))?;
    if let Some(text) = text {
        for ch in text.chars() {
            send_char(ch);
            // 每个字符都要过一遍引擎;给面板线程留出处理时间。
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    std::thread::sleep(Duration::from_millis(120));
    for key in keys {
        match key {
            Key::Enter => send_vk(VK_RETURN),
            Key::Esc => send_vk(VK_ESCAPE),
            Key::Down => send_vk(VK_DOWN),
        }
    }
    let executed = done_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "面板 10 秒内没有收起".to_string())?;
    let _ = spawned.join();
    Ok(executed)
}

/// 自检用按键。
#[derive(Clone, Copy)]
enum Key {
    Enter,
    Esc,
    Down,
}

/// 等面板窗口成为前台。
fn wait_panel_foreground(timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if foreground_class() == "lueying-command-panel" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("命令面板没有拿到前台".into())
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

fn foreground_class() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
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
        ONCE.call_once(|| unsafe {
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

/// 注入一个字符;Unicode 注入不依赖键盘布局。
fn send_char(ch: char) {
    let mut down = INPUT::default();
    down.r#type = INPUT_KEYBOARD;
    down.Anonymous.ki.wScan = ch as u16;
    down.Anonymous.ki.dwFlags = KEYEVENTF_UNICODE;
    let mut up = INPUT::default();
    up.r#type = INPUT_KEYBOARD;
    up.Anonymous.ki.wScan = ch as u16;
    up.Anonymous.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
    unsafe {
        SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32);
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

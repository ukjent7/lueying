//! Wayland 输入法状态机。
//!
//! `zwp_input_method_v2` 的 activate/done 序列驱动会话;键盘 grab 的每个
//! 按键先交给引擎,消费掉的按键不再下发,未消费的按键经虚拟键盘送回。
//! 候选窗是 popup surface,指针直接在窗内点选。

use std::collections::HashSet;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use calloop::LoopSignal;
use lueying_engine::{Event, Key};
use lueying_render::panel::Rect;
use lueying_render::toolbar::{Action, Status};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use xkbcommon::xkb::{self, KeyDirection, Keycode, Keymap, Keysym, State};

use crate::keymap::{self, Modifiers};
use crate::session::Session;
use crate::wayland::panel::Panel;
use crate::wayland::protocol::input_method::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2;
use crate::wayland::protocol::input_method::zwp_input_method_v2::ZwpInputMethodV2;
use crate::wayland::protocol::input_method::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2;
use crate::wayland::protocol::layer_shell::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use crate::wayland::protocol::layer_shell::zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1};
use crate::wayland::protocol::virtual_keyboard::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use crate::wayland::toolbar::Toolbar;
use crate::wayland::worker::{self, Command};

const BTN_LEFT: u32 = 0x110;

/// 缓冲归谁:候选窗与工具栏各自的缓冲释放时,走各自的分支。
#[derive(Clone, Copy, Debug)]
pub enum Owner {
    Panel,
    Toolbar,
}

/// 指针现在落在哪个窗口上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Nowhere,
    Panel,
    Toolbar,
}

struct Xkb {
    state: State,
}

struct Repeat {
    key: Key,
    evdev: u32,
    forward: bool,
}

/// 输入法协议对象。`keyboard_grab` 只为持有:丢弃即放开硬件键盘。
pub struct Protocol {
    pub input_method: ZwpInputMethodV2,
    pub keyboard_grab: ZwpInputMethodKeyboardGrabV2,
    pub virtual_keyboard: ZwpVirtualKeyboardV1,
}

pub struct App {
    pub session: Session,
    pub panel: Panel,
    protocol: Protocol,
    worker: Sender<Command>,
    qh: QueueHandle<App>,
    loop_signal: LoopSignal,
    xkb: Option<Xkb>,
    done_count: u32,
    active: bool,
    shift_tapped: bool,
    consumed: HashSet<u32>,
    repeat: Option<Repeat>,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_pos: (f32, f32),
    focus: Focus,
    toolbar: Option<Toolbar>,
    /// 输出的模式(物理像素)与缩放;工具栏用它算逻辑大小。
    output_mode: Option<(i32, i32)>,
    output_scale: i32,
}

impl App {
    pub fn new(
        session: Session,
        panel: Panel,
        protocol: Protocol,
        worker: Sender<Command>,
        qh: QueueHandle<App>,
        loop_signal: LoopSignal,
    ) -> Self {
        Self {
            session,
            panel,
            protocol,
            worker,
            qh,
            loop_signal,
            xkb: None,
            done_count: 0,
            active: false,
            shift_tapped: false,
            consumed: HashSet::new(),
            repeat: None,
            pointer: None,
            pointer_pos: (0.0, 0.0),
            focus: Focus::Nowhere,
            toolbar: None,
            output_mode: None,
            output_scale: 1,
        }
    }

    fn process(&mut self, key: Key) -> bool {
        self.session.set_now();
        let events = self.session.process_key(key);
        self.after_key();
        if events
            .iter()
            .any(|event| matches!(event, Event::Passthrough(_)))
        {
            return false;
        }
        let mut committed = String::new();
        for event in &events {
            if let Event::Commit(text) = event {
                committed.push_str(text);
            }
        }
        self.flush((!committed.is_empty()).then_some(committed.as_str()));
        let _ = self.worker.send(Command::Settle);
        true
    }

    /// 每个按键之后:角色看到一次按键,按钮上的字跟着状态走。
    fn after_key(&mut self) {
        if let Some(toolbar) = self.toolbar.as_mut() {
            toolbar.key_pressed();
        }
        self.sync_toolbar();
    }

    /// 把提交串与当前预编辑一次性发给客户端,并重画候选窗。
    fn flush(&mut self, commit: Option<&str>) {
        if let Some(text) = commit {
            self.protocol.input_method.commit_string(text.to_string());
        }
        let display = self
            .session
            .engine
            .composition()
            .map(|composition| composition.display.clone())
            .unwrap_or_default();
        let cursor = display.len() as i32;
        let input_method = &self.protocol.input_method;
        input_method.set_preedit_string(display, cursor, cursor);
        input_method.commit(self.done_count);
        self.redraw();
    }

    fn redraw(&mut self) {
        self.panel.draw(&self.session.view, &self.qh);
    }

    fn on_done(&mut self) {
        self.done_count = self.done_count.saturating_add(1);
    }

    fn key(&mut self, time: u32, evdev: u32, pressed: bool) {
        let Some(xkb) = self.xkb.as_mut() else {
            self.forward_key(time, evdev, pressed);
            return;
        };
        let keycode = Keycode::new(evdev + 8);
        xkb.state.update_key(
            keycode,
            if pressed {
                KeyDirection::Down
            } else {
                KeyDirection::Up
            },
        );
        let sym = xkb.state.key_get_one_sym(keycode);
        let text = xkb.state.key_get_utf8(keycode);
        let mods = Modifiers::from_state(&xkb.state);
        if is_shift(sym) {
            if pressed {
                self.shift_tapped = true;
            } else if self.shift_tapped {
                self.shift_tapped = false;
                self.process(Key::Shift);
            }
            self.forward_key(time, evdev, pressed);
            return;
        }
        if !pressed {
            let consumed = self.consumed.remove(&evdev);
            if consumed {
                if self
                    .repeat
                    .as_ref()
                    .is_some_and(|repeat| repeat.evdev == evdev)
                {
                    self.repeat = None;
                    let _ = self.worker.send(Command::KeyRelease);
                }
            } else {
                self.forward_key(time, evdev, pressed);
            }
            return;
        }
        self.shift_tapped = false;
        if is_modifier(sym) {
            self.forward_key(time, evdev, pressed);
            return;
        }
        if !self.active {
            self.forward_key(time, evdev, pressed);
            return;
        }
        let Some(key) = keymap::map(sym, &text, mods) else {
            self.forward_key(time, evdev, pressed);
            return;
        };
        if !self.process(key) {
            self.forward_key(time, evdev, pressed);
            return;
        }
        self.consumed.insert(evdev);
        self.repeat = Some(Repeat {
            key,
            evdev,
            forward: false,
        });
        let _ = self.worker.send(Command::KeyPress {
            repeat: repeats(key),
        });
    }

    fn repeat(&mut self) {
        let Some(repeat) = self.repeat.as_ref() else {
            return;
        };
        let key = repeat.key;
        let evdev = repeat.evdev;
        let forward = repeat.forward;
        if forward {
            self.forward_key(now_ms(), evdev, true);
        } else {
            self.process(key);
        }
    }

    fn forward_key(&self, time: u32, evdev: u32, pressed: bool) {
        let keyboard = &self.protocol.virtual_keyboard;
        keyboard.key(time, evdev, if pressed { 1 } else { 0 });
    }

    fn install_keymap(&mut self, fd: OwnedFd, size: u32, format: WEnum<wl_keyboard::KeymapFormat>) {
        let WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) = format else {
            return;
        };
        // SAFETY:fd 由合成器给出,随后立即被 xkbcommon 映射并在本函数内消费。
        let keymap = unsafe {
            Keymap::new_from_fd(
                &xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
                fd,
                size as usize,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        }
        .ok()
        .flatten();
        let Some(keymap) = keymap else {
            return;
        };
        let mut bytes = keymap
            .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1)
            .into_bytes();
        bytes.push(0);
        if let Some(fd) = create_fd("lueying-keymap", &bytes) {
            let keyboard = &self.protocol.virtual_keyboard;
            keyboard.keymap(1, fd.as_fd(), bytes.len() as u32);
        }
        self.xkb = Some(Xkb {
            state: State::new(&keymap),
        });
    }

    fn modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if let Some(xkb) = self.xkb.as_mut() {
            xkb.state
                .update_mask(depressed, latched, locked, 0, 0, group);
        }
        let keyboard = &self.protocol.virtual_keyboard;
        keyboard.modifiers(depressed, latched, locked, group);
    }

    fn hover(&mut self) {
        let hit = self.panel.hit(self.pointer_pos.0, self.pointer_pos.1);
        let hover = match hit {
            lueying_render::panel::Hit::Item(index) => Some(index),
            _ => None,
        };
        if self.panel.set_hover(hover) {
            self.redraw();
        }
    }

    fn click(&mut self) {
        match self.panel.hit(self.pointer_pos.0, self.pointer_pos.1) {
            lueying_render::panel::Hit::Item(index) => {
                let text = self.session.pick(index);
                self.flush(text.as_deref());
            }
            lueying_render::panel::Hit::PrevPage => {
                if self.session.page(-1) {
                    self.flush(None);
                }
            }
            lueying_render::panel::Hit::NextPage => {
                if self.session.page(1) {
                    self.flush(None);
                }
            }
            lueying_render::panel::Hit::Nothing => {}
        }
    }

    /// 建工具栏:输出的大小要先拿到(`output_mode`),合成器没有 layer-shell 时不建。
    pub fn start_toolbar(
        &mut self,
        compositor: &wl_compositor::WlCompositor,
        shell: &ZwlrLayerShellV1,
        shm: &wl_shm::WlShm,
    ) {
        let Some((width, height)) = self.output_mode else {
            eprintln!("lueying-linux: 没有拿到输出大小,工具栏不显示");
            return;
        };
        let scale = self.output_scale.max(1) as f32;
        let screen = Rect::from_xywh(0.0, 0.0, width as f32 / scale, height as f32 / scale);
        self.toolbar = Some(Toolbar::new(
            compositor,
            shell,
            shm.clone(),
            screen,
            &self.qh,
        ));
    }

    pub fn has_toolbar(&self) -> bool {
        self.toolbar.is_some()
    }

    /// 工具栏的逐帧回调:有角色在动就重画。
    pub fn toolbar_frame(&mut self) {
        let Some(toolbar) = self.toolbar.as_mut() else {
            return;
        };
        if toolbar.tick() {
            toolbar.redraw(&self.qh);
        }
    }

    /// 按钮上的字跟着会话状态走。
    fn sync_toolbar(&mut self) {
        let status: Status = self.session.status();
        if let Some(toolbar) = self.toolbar.as_mut() {
            toolbar.set_status(status, &self.qh);
        }
    }

    /// 工具栏按钮的动作。模式复用 Shift 的效果;简繁、标点、全半角改会话;设置打开配置文件。
    fn run_toolbar_action(&mut self, action: Action) {
        match action {
            Action::ToggleMode => {
                self.process(Key::Shift);
            }
            Action::ToggleFullWidth => self.session.toggle_fullwidth(),
            Action::TogglePunct => self.session.toggle_punct(),
            Action::ToggleScript => self.session.toggle_script(),
            Action::OpenSettings => crate::config::open_for_editing(),
            Action::Hide => {
                if let Some(toolbar) = self.toolbar.as_mut() {
                    toolbar.collapse();
                }
            }
        }
        self.sync_toolbar();
        self.redraw();
    }

    fn toolbar_press(&mut self) {
        let pos = self.pointer_pos;
        if let Some(toolbar) = self.toolbar.as_mut() {
            toolbar.press(pos);
        }
    }

    fn toolbar_motion(&mut self) {
        let pos = self.pointer_pos;
        if let Some(toolbar) = self.toolbar.as_mut() {
            toolbar.motion(pos);
        }
    }

    fn toolbar_release(&mut self) {
        let pos = self.pointer_pos;
        let Some(button) = self
            .toolbar
            .as_mut()
            .and_then(|toolbar| toolbar.release(pos))
        else {
            return;
        };
        self.run_toolbar_action(button.action());
    }

    pub fn on_worker(&mut self, event: worker::Event) {
        match event {
            worker::Event::Settle => {
                self.session.settle();
                self.redraw();
            }
            worker::Event::Repeat => self.repeat(),
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                surface,
                ..
            } => {
                state.pointer_pos = (surface_x as f32, surface_y as f32);
                state.focus = if surface.id() == state.panel.surface().id() {
                    Focus::Panel
                } else if state.toolbar.as_ref().is_some_and(|t| t.is(&surface)) {
                    Focus::Toolbar
                } else {
                    Focus::Nowhere
                };
                if state.focus == Focus::Panel {
                    state.hover();
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_pos = (surface_x as f32, surface_y as f32);
                match state.focus {
                    Focus::Panel if !state.panel.hidden() => state.hover(),
                    Focus::Toolbar => state.toolbar_motion(),
                    _ => {}
                }
            }
            wl_pointer::Event::Leave { .. } => {
                if state.focus == Focus::Panel && state.panel.set_hover(None) {
                    state.redraw();
                }
                state.focus = Focus::Nowhere;
            }
            wl_pointer::Event::Button {
                button,
                state: button_state,
                ..
            } if button == BTN_LEFT => {
                let pressed =
                    matches!(button_state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                match (state.focus, pressed) {
                    (Focus::Panel, true) => state.click(),
                    (Focus::Toolbar, true) => state.toolbar_press(),
                    (Focus::Toolbar, false) => state.toolbar_release(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Pointer)
            && state.pointer.is_none()
        {
            state.pointer = Some(seat.get_pointer(qh, ()));
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, Owner> for App {
    fn event(
        state: &mut Self,
        _proxy: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        owner: &Owner,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            match owner {
                Owner::Panel => {
                    state.panel.release();
                    if state.panel.take_dirty() {
                        state.redraw();
                    }
                }
                Owner::Toolbar => {
                    if let Some(toolbar) = state.toolbar.as_mut() {
                        toolbar.buffer_released();
                        if toolbar.take_dirty() {
                            toolbar.redraw(&state.qh);
                        }
                    }
                }
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                if matches!(flags, WEnum::Value(mode) if mode.contains(wl_output::Mode::Current)) {
                    state.output_mode = Some((width, height));
                }
            }
            wl_output::Event::Scale { factor } => state.output_scale = factor,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrLayerShellV1,
        _event: crate::wayland::protocol::layer_shell::zwlr_layer_shell_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                if let Some(toolbar) = state.toolbar.as_mut() {
                    toolbar.configure(serial, qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => state.toolbar = None,
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpInputPopupSurfaceV2,
        _event: crate::wayland::protocol::input_method::zwp_input_popup_surface_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputMethodKeyboardGrabV2,
        event: crate::wayland::protocol::input_method::zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::wayland::protocol::input_method::zwp_input_method_keyboard_grab_v2::Event;
        match event {
            Event::Keymap { format, fd, size } => state.install_keymap(fd, size, format),
            Event::Key {
                time,
                key,
                state: key_state,
                ..
            } => {
                let pressed = matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                state.key(time, key, pressed);
            }
            Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => state.modifiers(mods_depressed, mods_latched, mods_locked, group),
            Event::RepeatInfo { rate, delay } => {
                let _ = state.worker.send(Command::RepeatInfo { rate, delay });
            }
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputMethodV2,
        event: crate::wayland::protocol::input_method::zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use crate::wayland::protocol::input_method::zwp_input_method_v2::Event;
        match event {
            Event::Activate => {
                state.active = true;
                state.session.refresh();
                state.redraw();
                if let Some(toolbar) = state.toolbar.as_mut() {
                    toolbar.expand();
                }
            }
            Event::Deactivate => {
                state.active = false;
                state.consumed.clear();
                state.repeat = None;
                state.shift_tapped = false;
                let _ = state.worker.send(Command::KeyRelease);
                state.session.deactivate();
                state.panel.hide();
                state.session.save();
            }
            Event::SurroundingText { text, cursor, .. } => {
                state.session.set_context(&text, cursor);
            }
            Event::Done => state.on_done(),
            Event::Unavailable => {
                eprintln!("lueying-linux: 合成器已停用输入法对象");
                state.loop_signal.stop();
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: crate::wayland::protocol::virtual_keyboard::zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<crate::wayland::protocol::virtual_keyboard::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &crate::wayland::protocol::virtual_keyboard::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        _event: crate::wayland::protocol::virtual_keyboard::zwp_virtual_keyboard_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<crate::wayland::protocol::input_method::zwp_input_method_manager_v2::ZwpInputMethodManagerV2, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &crate::wayland::protocol::input_method::zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        _event: crate::wayland::protocol::input_method::zwp_input_method_manager_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

fn is_shift(sym: Keysym) -> bool {
    matches!(sym, Keysym::Shift_L | Keysym::Shift_R)
}

fn is_modifier(sym: Keysym) -> bool {
    matches!(
        sym,
        Keysym::Shift_L
            | Keysym::Shift_R
            | Keysym::Control_L
            | Keysym::Control_R
            | Keysym::Alt_L
            | Keysym::Alt_R
            | Keysym::Super_L
            | Keysym::Super_R
            | Keysym::Meta_L
            | Keysym::Meta_R
            | Keysym::Caps_Lock
            | Keysym::Num_Lock
    )
}

fn repeats(key: Key) -> bool {
    matches!(
        key,
        Key::Backspace
            | Key::Delete
            | Key::SegmentBackspace
            | Key::SegmentDelete
            | Key::Left
            | Key::Right
            | Key::SegmentLeft
            | Key::SegmentRight
            | Key::Up
            | Key::Down
            | Key::PageUp
            | Key::PageDown
    )
}

fn create_fd(name: &str, bytes: &[u8]) -> Option<OwnedFd> {
    let mut file = memfd::MemfdOptions::default()
        .create(name)
        .ok()?
        .into_file();
    file.write_all(bytes).ok()?;
    Some(OwnedFd::from(file))
}

fn now_ms() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u32)
}

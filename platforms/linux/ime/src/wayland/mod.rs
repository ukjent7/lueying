//! Wayland 输入法进程入口。

pub mod app;
pub mod panel;
pub mod protocol;
pub mod toolbar;
pub mod worker;

use std::error::Error;

use calloop::EventLoop;
use calloop::timer::{TimeoutAction, Timer};
use calloop_wayland_source::WaylandSource;
use wayland_client::Connection;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_compositor, wl_output, wl_seat, wl_shm};

use crate::session::Session;
use app::{App, Protocol};
use panel::Panel;
use protocol::input_method::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use protocol::layer_shell::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use protocol::virtual_keyboard::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;

pub fn run() -> Result<(), Box<dyn Error>> {
    let config = crate::config::load();
    let session = Session::new(&config);
    let connection = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<App>(&connection)?;
    let qh = event_queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=6, ())?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ())?;
    let input_method_manager: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ())?;
    let virtual_keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ())?;
    // 工具栏可有可无:没有输出或合成器不支持 layer-shell(如 GNOME)时只是不显示工具栏。
    let output: Option<wl_output::WlOutput> = globals.bind(&qh, 2..=4, ()).ok();
    let layer_shell: Option<ZwlrLayerShellV1> = globals.bind(&qh, 1..=4, ()).ok();

    let input_method = input_method_manager.get_input_method(&seat, &qh, ());
    let keyboard_grab = input_method.grab_keyboard(&qh, ());
    let virtual_keyboard = virtual_keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let popup = input_method.get_input_popup_surface(&surface, &qh, ());
    let panel = Panel::new(surface, popup, shm, crate::theme::current());

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let loop_signal = event_loop.get_signal();
    let (worker_events, worker_channel) = calloop::channel::channel();
    let worker = worker::spawn(worker_events);
    let protocol = Protocol {
        input_method,
        keyboard_grab,
        virtual_keyboard,
    };
    let mut app = App::new(session, panel, protocol, worker, qh, loop_signal);
    event_queue.roundtrip(&mut app)?;
    if let (Some(shell), Some(_)) = (layer_shell.as_ref(), output.as_ref()) {
        app.start_toolbar(&compositor, shell, &shm);
        event_queue.roundtrip(&mut app)?;
    }

    let handle = event_loop.handle();
    handle
        .insert_source(worker_channel, |event, _, app| {
            if let calloop::channel::Event::Msg(event) = event {
                app.on_worker(event);
            }
        })
        .map_err(|error| std::io::Error::other(format!("注册定时器失败: {error}")))?;
    // 只有工具栏存在时才需要逐帧回调,没有就不唤醒事件循环。
    if app.has_toolbar() {
        handle
            .insert_source(Timer::from_duration(toolbar::FRAME), |_, _, app| {
                app.toolbar_frame();
                TimeoutAction::ToDuration(toolbar::FRAME)
            })
            .map_err(|error| std::io::Error::other(format!("注册工具栏定时器失败: {error}")))?;
    }
    WaylandSource::new(connection, event_queue)
        .insert(handle)
        .map_err(|error| std::io::Error::other(format!("注册 Wayland 事件源失败: {error}")))?;
    event_loop.run(None, &mut app, |_| {})?;
    app.session.save();
    Ok(())
}

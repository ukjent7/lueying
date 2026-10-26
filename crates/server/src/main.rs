//! lueying-server:常驻服务进程。持有词典(一份)与每个宿主线程一个引擎会话,
//! 经命名管道接收 TSF DLL 的按键请求并返回组句/上屏。候选窗 UI 随后接入。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::real_main();
    #[cfg(not(windows))]
    println!("lueying-server 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, RwLock, mpsc};
    use std::time::Instant;

    use lueying_dict::Dictionary;
    use lueying_dict::gram::Gram;
    use lueying_engine::{CivilTime, Engine, Event, Key, Mode, SharedUser, UserDict};
    use lueying_protocol::{Reply, Request, SessionView, pipe_name};
    use lueying_rerank::{Model, Rerank};
    use lueying_server::config::{self, AppConfig};
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows::core::HSTRING;

    /// 当前系统本地时间。日期/时间候选按宿主时区生成,不靠引擎猜 UTC。
    fn local_time() -> CivilTime {
        let st = unsafe { GetLocalTime() };
        CivilTime {
            year: i32::from(st.wYear),
            month: u32::from(st.wMonth),
            day: u32::from(st.wDay),
            hour: u32::from(st.wHour),
            minute: u32::from(st.wMinute),
            second: u32::from(st.wSecond),
            // SYSTEMTIME.wDayOfWeek: 0 = 星期日 … 6 = 星期六。
            weekday: (u32::from(st.wDayOfWeek) + 6) % 7 + 1,
        }
    }

    pub(super) struct Session {
        pub(super) engine: Engine,
        pub(super) view: SessionView,
        /// `Key` 的预演结果,`TakeKey` 取走(DLL 在 OnKeyDown 应用到文档)。
        pub(super) pending: Option<Reply>,
        /// 候选窗鼠标点选/翻页产生的待写回内容,TSF 的 `Sync` 取走。
        pub(super) ui_reply: Option<Reply>,
        /// 4M 每键同步重排,前缀缓存随会话存活。
        pub(super) sync_rerank: Option<Rerank>,
        /// 25M 停顿重排,异步线程在锁内推理,不占 `sessions` 锁。
        pub(super) settled_rerank: Option<Arc<Mutex<Rerank>>>,
        /// 宿主已接管候选绘制;候选窗让位给宿主/读屏。
        pub(super) host_draws: bool,
        /// 最近一次 Context 的焦点代次;异步 UIA 结果据此丢弃过期文本。
        pub(super) context_epoch: u64,
        /// 每次 Context 自增;同一代次里连开两次组合也能区分旧查询。
        pub(super) context_seq: u64,
    }

    /// 一次用户主动的模式变化;候选窗同进程的徽标据此短暂显示「中」「英」「全」「半」。
    pub(super) struct ModeHud {
        pub(super) label: &'static str,
        /// 切换时的宿主光标矩形(物理像素);没有可用光标时退到主屏。
        pub(super) caret: Option<(i32, i32, i32, i32)>,
        pub(super) at: Instant,
    }

    pub(super) struct State {
        pub(super) dict: Arc<Dictionary>,
        pub(super) gram: Option<Arc<Gram>>,
        pub(super) sessions: Mutex<HashMap<u64, Session>>,
        /// 会话 → 当前管道的连接号。宿主崩溃后按连接清理,避免会话与缓存常驻。
        pub(super) conns: Mutex<HashMap<u64, u64>>,
        /// 每次会话变化自增,候选窗轮询它决定刷新。
        pub(super) version: AtomicU64,
        /// 每次中英切换自增,模式徽标轮询它决定是否重新显示。
        pub(super) hud_serial: AtomicU64,
        pub(super) mode_hud: Mutex<Option<ModeHud>>,
        /// 最近收到按键/光标上报的会话,候选窗只跟它。
        pub(super) active: AtomicU64,
        pub(super) sync_model: Option<Model>,
        pub(super) settled_model: Option<Model>,
        pub(super) settle_tx: Option<mpsc::Sender<crate::settle::SettleMessage>>,
        /// TSF 读不到左邻文本时交给 UIA 线程补读;容量 1,忙时丢新任务。
        pub(super) uia_tx: Option<mpsc::SyncSender<crate::uia::Job>>,
        /// 所有会话共用一份学习结果。
        pub(super) user: SharedUser,
        /// 设置窗保存的配置;新会话取它,重载时下发给所有会话。
        pub(super) config: RwLock<AppConfig>,
        pub(super) config_path: Option<PathBuf>,
        /// 全局中/英模式:所有应用共享,微软拼音同款;新会话上线时跟随。
        pub(super) chinese: AtomicBool,
        /// 全局全角/半角:Shift+Space 或系统转换模式切换;新会话上线时跟随。
        pub(super) fullwidth: AtomicBool,
    }

    pub(super) fn real_main() {
        // 候选窗坐标来自 TSF 的物理像素光标矩形;进程必须 Per-Monitor V2,
        // 否则 SetWindowPos 会被系统按 96 DPI 虚拟化,副屏与高缩放下会偏移。
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        // 同一登录会话只允许一个 Server:否则两个进程会各建管道实例,
        // 用户的词库学习和候选窗状态会分裂到不同进程。
        let Some(_instance) = single_instance() else {
            println!("已有 lueying-server 在运行,本进程退出");
            return;
        };
        // 三档降级:二进制(mmap 零解析) → 文本(兼容旧数据) → 内置迷你词库。
        // 对应 WeType 的 "safe mode, just load buildin dict":词典坏了输入法仍可用。
        let dict = ["main.pyd", "main.tsv"]
            .iter()
            .find_map(|name| dict_path(name).and_then(load_dict))
            .unwrap_or_else(|| {
                println!("主词典缺失或损坏,退回内置迷你词库");
                Dictionary::builtin()
            });
        // 语言模型是可选数据:缺失或格式不符就退化为纯词频,不影响输入。
        let gram = dict_path("main.gram").and_then(|p| match Gram::open(&p) {
            Ok(g) => {
                println!("语法模型 ← {}", p.display());
                Some(Arc::new(g))
            }
            Err(e) => {
                println!("语法模型 {} 不可用: {e}", p.display());
                None
            }
        });
        // 首次整句解码要算词频总和;放在启动时空转一次,别让首键承担。
        let _ = dict.total_weight();
        // 两档神经重排都是可选数据:缺失/损坏就只保留词频候选。
        let sync_model = load_model("rerank-4m.safetensors");
        let settled_model = load_model("rerank-25m.safetensors");
        // 用户词库是可选数据:读不到就从空开始,坏行跳过。
        let user_path = crate::user_store::path();
        let user = Arc::new(RwLock::new(match &user_path {
            Some(path) => crate::user_store::load(path),
            None => {
                println!("用户词库不可持久化:LOCALAPPDATA 缺失");
                UserDict::new()
            }
        }));
        if let Some(path) = user_path {
            crate::user_store::spawn(Arc::clone(&user), path);
        }
        // 用户配置:缺失或坏行就用默认值。
        let config_path = config::path();
        let app_config = config_path
            .as_ref()
            .map_or_else(AppConfig::default, |path| config::load(path));
        if let Some(path) = &config_path {
            println!("配置 ← {}", path.display());
        }
        let (settle_tx, settle_rx) = mpsc::channel();
        let settle_tx = settled_model.as_ref().map(|_| settle_tx);
        let (uia_tx, uia_rx) = mpsc::sync_channel::<crate::uia::Job>(1);
        let state = Arc::new(State {
            dict: Arc::new(dict),
            gram,
            sessions: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
            version: AtomicU64::new(0),
            hud_serial: AtomicU64::new(0),
            mode_hud: Mutex::new(None),
            active: AtomicU64::new(0),
            sync_model,
            settled_model,
            settle_tx,
            uia_tx: Some(uia_tx),
            user,
            config: RwLock::new(app_config),
            config_path,
            chinese: AtomicBool::new(true),
            fullwidth: AtomicBool::new(false),
        });
        if state.settle_tx.is_some() {
            crate::settle::spawn(Arc::clone(&state), settle_rx);
        }
        crate::uia::spawn(Arc::clone(&state), uia_rx);
        let pipe = pipe_name();
        println!("lueying-server 监听 {pipe}");
        {
            // 管道服务在后台线程,主线程留给 gpui 事件循环。
            let state = Arc::clone(&state);
            std::thread::spawn(move || crate::pipe::listen(&pipe, state));
        }
        // 无桌面会话(CI/服务)没有窗口站,GPUI 建窗会失败;输入法本身只依赖管道。
        if std::env::var_os("LUEYING_SERVER_NO_UI").is_some() {
            loop {
                std::thread::park();
            }
        }
        let ui_state = Arc::clone(&state);
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::ui::run(ui_state);
        }))
        .is_err()
        {
            println!("候选窗不可用,继续无界面服务");
        }
        // 窗口循环意外结束时进程也要留着,否则整个输入法跟着退出。
        loop {
            std::thread::park();
        }
    }

    /// 当前会话的单实例锁;返回的句柄要活到进程结束。
    fn single_instance() -> Option<HANDLE> {
        let pipe = pipe_name();
        let short = pipe.trim_start_matches(r"\\.\pipe\");
        let name = HSTRING::from(format!("Local\\{short}"));
        let handle = unsafe { CreateMutexW(None, true, &name) }.ok()?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }
        Some(handle)
    }

    /// exe 同目录的数据文件(main.pyd / main.tsv / main.gram / rerank-*.safetensors)。
    fn dict_path(name: &str) -> Option<std::path::PathBuf> {
        let mut path = std::env::current_exe().ok()?;
        path.set_file_name(name);
        path.exists().then_some(path)
    }

    fn load_model(name: &str) -> Option<Model> {
        let path = dict_path(name)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                println!("重排模型 {} 读取失败: {e}", path.display());
                return None;
            }
        };
        match Model::load(&bytes) {
            Ok(model) => {
                println!(
                    "重排模型 {} ← {}（{} 字节）",
                    name,
                    path.display(),
                    bytes.len()
                );
                Some(model)
            }
            Err(e) => {
                println!("重排模型 {} 不可用: {e}", path.display());
                None
            }
        }
    }

    fn load_dict(path: std::path::PathBuf) -> Option<Dictionary> {
        match Dictionary::from_path(&path) {
            Ok((d, 0)) if !d.is_empty() => {
                println!("词典 {} 条 ← {}", d.len(), path.display());
                Some(d)
            }
            Ok((_, bad)) => {
                println!("{} 有 {bad} 处问题,跳过", path.display());
                None
            }
            Err(e) => {
                println!("{} 加载失败: {e}", path.display());
                None
            }
        }
    }

    /// 处理一条请求,返回应答。
    pub(super) fn handle(state: &State, req: Request) -> Reply {
        // 任何请求后候选窗都可能要重摆,先记账再处理;只读上报不触发重绘。
        if !matches!(&req, Request::Sync { .. } | Request::Context { .. }) {
            state.version.fetch_add(1, Ordering::Relaxed);
        }
        let mut sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match req {
            Request::Hello { session, host } => {
                println!("会话 {session} 上线({host})");
                // 同一线程号重新上线(Server 重启、DLL 重连)必须从头开始,
                // 不能继承上一个线程留下的组合与待办。
                sessions.remove(&session);
                crate::settle::cancel(state, session);
                let mut engine = {
                    let cfg = state
                        .config
                        .read()
                        .unwrap_or_else(|p| p.into_inner())
                        .engine();
                    Engine::new(Arc::clone(&state.dict), cfg).with_user(Arc::clone(&state.user))
                };
                if let Some(g) = &state.gram {
                    engine = engine.with_gram(Arc::clone(g));
                }
                engine.set_mode(current_mode(state));
                engine.set_fullwidth(state.fullwidth.load(Ordering::Relaxed));
                engine.set_now(local_time());
                let chinese = engine.mode() == Mode::Chinese;
                sessions.insert(
                    session,
                    Session {
                        engine,
                        view: SessionView {
                            host,
                            chinese,
                            fullwidth: state.fullwidth.load(Ordering::Relaxed),
                            ..SessionView::default()
                        },
                        pending: None,
                        ui_reply: None,
                        sync_rerank: state.sync_model.as_ref().map(Rerank::new),
                        settled_rerank: state
                            .settled_model
                            .as_ref()
                            .map(|m| Arc::new(Mutex::new(Rerank::new(m)))),
                        host_draws: false,
                        context_epoch: 0,
                        context_seq: 0,
                    },
                );
                state.active.store(session, Ordering::Relaxed);
                neutral(state)
            }
            Request::Key { session, key } => {
                state.active.store(session, Ordering::Relaxed);
                let Some(s) = sessions.get_mut(&session) else {
                    // 会话已经下线(Server 刚重启/宿主走了 Goodbye):让键直通,
                    // 不能返回一个「已消费」的空应答把按键吞掉。
                    return Reply {
                        passthrough: true,
                        chinese: state.chinese.load(Ordering::Relaxed),
                        fullwidth: state.fullwidth.load(Ordering::Relaxed),
                        ..Reply::default()
                    };
                };
                // 鼠标点选可能还没被 TSF 的 Sync 取走;先记下,和本次按键的
                // 结果一起写,避免快速连打把点选上屏吞掉。
                let carried = s.ui_reply.take();
                let engine_key: Key = key.into();
                let shift_toggle = state
                    .config
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .shift_toggle;
                if engine_key == Key::Shift && !shift_toggle {
                    // 不切模式,但要把当前内联文本原样带回,免得 TSF 清掉组合。
                    let mut reply = current_reply(s);
                    if let Some(carried) = carried {
                        let mut commits = carried.commits;
                        commits.extend(reply.commits);
                        reply.commits = commits;
                    }
                    return reply;
                }
                let before_mode = s.engine.mode();
                let before_rev = s.engine.revision();
                // 每次按键刷新本地时间,跨午夜/夏令时切换后日期仍然正确。
                s.engine.set_now(local_time());
                let events = s.engine.process(engine_key);
                // 只有候选集合重算过才重排;翻页/移动高亮不能把用户的选择重置。
                if s.engine.revision() != before_rev {
                    apply_sync_rerank(s);
                }
                let mut reply = reply_from(s, &events);
                if let Some(carried) = carried {
                    if reply.passthrough {
                        // 直通键不经过编辑会话,点选结果留给下一次 Sync。
                        s.ui_reply = Some(carried);
                    } else {
                        let mut commits = carried.commits;
                        commits.extend(reply.commits);
                        reply.commits = commits;
                    }
                }
                refresh_view(s);
                fill_view(&mut reply, s);
                s.pending = Some(reply.clone());
                crate::settle::schedule(state, session, s);
                let mode_changed = s.engine.mode() != before_mode;
                let chinese = s.engine.mode() == Mode::Chinese;
                if mode_changed {
                    set_global_mode(state, &mut sessions, chinese);
                }
                reply
            }
            Request::ToggleMode { session } => {
                state.active.store(session, Ordering::Relaxed);
                let Some(s) = sessions.get_mut(&session) else {
                    return neutral(state);
                };
                let carried = s.ui_reply.take();
                let before_rev = s.engine.revision();
                let events = s.engine.process(Key::Shift);
                if s.engine.revision() != before_rev {
                    apply_sync_rerank(s);
                }
                let mut reply = reply_from(s, &events);
                if let Some(carried) = carried {
                    let mut commits = carried.commits;
                    commits.extend(reply.commits);
                    reply.commits = commits;
                }
                // 模式切换可能已把组合落定,之前缓存的按键应答不能再写回。
                s.pending = None;
                refresh_view(s);
                fill_view(&mut reply, s);
                crate::settle::schedule(state, session, s);
                let chinese = s.engine.mode() == Mode::Chinese;
                set_global_mode(state, &mut sessions, chinese);
                reply
            }
            Request::Sync { session } => {
                let Some(s) = sessions.get_mut(&session) else {
                    return neutral(state);
                };
                let mut reply = s.ui_reply.take().unwrap_or_default();
                reply.preedit = s.view.preedit.clone();
                reply.chinese = state.chinese.load(Ordering::Relaxed);
                fill_view(&mut reply, s);
                reply
            }
            Request::Context {
                session,
                text,
                private,
                fallback,
                hwnd,
                epoch,
            } => {
                let mut want_uia = false;
                let mut uia_context = String::new();
                let mut uia_seq = 0;
                if let Some(s) = sessions.get_mut(&session) {
                    // 输入范围每次焦点/组合开始都重报;私密字段不学、不记前文。
                    s.engine.set_learning(!private);
                    s.context_epoch = epoch;
                    s.context_seq = s.context_seq.wrapping_add(1);
                    uia_seq = s.context_seq;
                    if private {
                        s.engine.clear_context();
                    } else {
                        apply_context(state, session, s, &text);
                    }
                    want_uia = !private && fallback && hwnd != 0;
                    if want_uia {
                        // UIA 回来时若这里已经因为上屏而变化,就丢弃旧文本。
                        uia_context = s.engine.context().to_string();
                    }
                }
                if private {
                    crate::settle::cancel(state, session);
                } else if want_uia
                    && let Some(tx) = state.uia_tx.as_ref()
                {
                    // 空上下文已经先应用;UIA 结果回来再按代次/页/高亮决定
                    // 是否采用。容量 1,前一次查询没回来时直接丢新任务。
                    let _ = tx.try_send(crate::uia::Job {
                        session,
                        hwnd,
                        epoch,
                        seq: uia_seq,
                        context: uia_context,
                    });
                }
                Reply::default()
            }
            Request::TakeKey { session } => sessions
                .get_mut(&session)
                .and_then(|s| s.pending.take())
                .unwrap_or_else(|| neutral(state)),
            Request::SetMode { session, chinese } => {
                state.active.store(session, Ordering::Relaxed);
                let reply = if let Some(s) = sessions.get_mut(&session) {
                    let carried = s.ui_reply.take();
                    let before_rev = s.engine.revision();
                    // 组合中切模式要先把拼音落定,不能直接清掉用户敲了一半的内容。
                    let events = switch_mode(s, chinese);
                    if s.engine.revision() != before_rev {
                        apply_sync_rerank(s);
                    }
                    let mut reply = reply_from(s, &events);
                    if let Some(carried) = carried {
                        let mut commits = carried.commits;
                        commits.extend(reply.commits);
                        reply.commits = commits;
                    }
                    // 切模式前的按键应答已过期,避免 OnKeyDown 把旧内联文本写回。
                    s.pending = None;
                    refresh_view(s);
                    fill_view(&mut reply, s);
                    crate::settle::schedule(state, session, s);
                    reply
                } else {
                    neutral(state)
                };
                set_global_mode(state, &mut sessions, chinese);
                reply
            }
            Request::ToggleFullwidth { session } => {
                state.active.store(session, Ordering::Relaxed);
                if sessions.contains_key(&session) {
                    let fullwidth = !state.fullwidth.load(Ordering::Relaxed);
                    set_global_fullwidth(state, &mut sessions, fullwidth);
                }
                neutral(state)
            }
            Request::SetFullwidth { session, fullwidth } => {
                state.active.store(session, Ordering::Relaxed);
                if !sessions.contains_key(&session) {
                    return neutral(state);
                }
                set_global_fullwidth(state, &mut sessions, fullwidth);
                // 系统 compartment 的回调可能正组着句;带上当前内联文本,
                // 让 TSF 的 apply_reply 重写同一份预编辑而不是把它清掉。
                let mut reply = neutral(state);
                if let Some(s) = sessions.get(&session) {
                    reply.preedit = s.view.preedit.clone();
                    fill_view(&mut reply, s);
                }
                reply
            }
            Request::Focus { session, focused } => {
                // 失焦通知可能晚于新窗口的聚焦通知;只允许聚焦者抢占 active,
                // 失焦只清理自己,避免候选窗跟到已经切走的旧输入框。
                if focused {
                    state.active.store(session, Ordering::Relaxed);
                } else if state.active.load(Ordering::Relaxed) == session {
                    state.active.store(0, Ordering::Relaxed);
                }
                let mut reply = neutral(state);
                if let Some(s) = sessions.get_mut(&session) {
                    // 失焦/换输入框时作废在途的 UIA 读取;新 Context 会带来新代次。
                    s.context_epoch = s.context_epoch.wrapping_add(1);
                    s.context_seq = s.context_seq.wrapping_add(1);
                    s.pending = None;
                    let carried = s.ui_reply.take();
                    if !focused {
                        // 失焦:把敲了一半的拼音整串落定,再清上下文。
                        // 只按一次空格会留下吃前缀候选之后的残余拼音。
                        let events = s.engine.flush();
                        reply = reply_from(s, &events);
                        if let Some(carried) = carried {
                            let mut commits = carried.commits;
                            commits.extend(reply.commits);
                            reply.commits = commits;
                        }
                        s.engine.clear_context();
                        crate::settle::cancel(state, session);
                    } else {
                        // 回到本线程/新输入框:若上一次失焦通知丢了,残留组合
                        // 不能带进新上下文。正常失焦已由上面的 false 分支落定。
                        if s.engine.composition().is_some() {
                            let _ = s.engine.process(Key::Esc);
                        }
                        // 点击候选后应用可能强行结束组句;保留待写文本,
                        // 下一次按键或 Sync 再补进文档。
                        s.ui_reply = carried;
                        s.engine.clear_context();
                    }
                    refresh_view(s);
                    fill_view(&mut reply, s);
                    // 清组合/换输入框后,待写回的内联文本必须跟上当前状态。
                    sync_ui_reply(s);
                }
                reply
            }
            Request::CompositionTerminated { session } => {
                if let Some(s) = sessions.get_mut(&session) {
                    let _ = s.engine.process(Key::Esc);
                    // 已点选、还没写回的文本不能丢;下次按键/Sync 继续带上。
                    crate::settle::cancel(state, session);
                    refresh_view(s);
                    sync_ui_reply(s);
                }
                neutral(state)
            }
            Request::Caret {
                session,
                left,
                top,
                right,
                bottom,
            } => {
                state.active.store(session, Ordering::Relaxed);
                if let Some(s) = sessions.get_mut(&session) {
                    s.view.caret = Some((left, top, right, bottom));
                    // 组合已经建立、上下文也可能刚上报;在这里排停顿任务最可靠。
                    crate::settle::schedule(state, session, s);
                }
                Reply::default()
            }
            Request::Pick { session, slot } => {
                state.active.store(session, Ordering::Relaxed);
                if let Some(s) = sessions.get_mut(&session) {
                    let before = s.engine.revision();
                    let events = s.engine.pick_in_page(slot);
                    if s.engine.revision() != before {
                        apply_sync_rerank(s);
                    }
                    refresh_view(s);
                    let commits: Vec<String> = events
                        .iter()
                        .filter_map(|e| match e {
                            Event::Commit(t) => Some(t.clone()),
                            _ => None,
                        })
                        .collect();
                    if !commits.is_empty() {
                        let chinese = s.view.chinese;
                        let fullwidth = s.view.fullwidth;
                        let pending = s.ui_reply.get_or_insert_with(|| Reply {
                            chinese,
                            fullwidth,
                            ..Reply::default()
                        });
                        pending.commits.extend(commits);
                        pending.preedit = s.view.preedit.clone();
                        pending.chinese = chinese;
                        pending.fullwidth = fullwidth;
                    }
                    crate::settle::schedule(state, session, s);
                }
                Reply::default()
            }
            Request::PickIndex { session, index } => {
                state.active.store(session, Ordering::Relaxed);
                let mut reply = Reply::default();
                if let Some(s) = sessions.get_mut(&session) {
                    let before = s.engine.revision();
                    let events = s.engine.pick(index);
                    if s.engine.revision() != before {
                        apply_sync_rerank(s);
                    }
                    // 宿主自绘候选时由 TSF 直接收走上屏文本并写进文档,
                    // 不走候选窗的 Sync 队列。
                    reply = reply_from(s, &events);
                    refresh_view(s);
                    fill_view(&mut reply, s);
                    crate::settle::schedule(state, session, s);
                }
                reply
            }
            Request::Finalize { session } => {
                state.active.store(session, Ordering::Relaxed);
                let Some(s) = sessions.get_mut(&session) else {
                    return neutral(state);
                };
                let carried = s.ui_reply.take();
                let mut reply = Reply {
                    chinese: s.engine.mode() == Mode::Chinese,
                    ..Reply::default()
                };
                if let Some(text) = s.engine.finalize_display() {
                    reply.commits.push(text);
                }
                if let Some(carried) = carried {
                    let mut commits = carried.commits;
                    commits.extend(reply.commits);
                    reply.commits = commits;
                }
                // 组合已经结束,之前的按键应答不能再写回内联拼音。
                s.pending = None;
                refresh_view(s);
                fill_view(&mut reply, s);
                crate::settle::schedule(state, session, s);
                reply
            }
            Request::SelectIndex { session, index } => {
                state.active.store(session, Ordering::Relaxed);
                let mut reply = Reply::default();
                if let Some(s) = sessions.get_mut(&session) {
                    if s.engine.set_highlight(index) {
                        // 用户正在挑候选,停顿重排不能把高亮挪走。
                        crate::settle::cancel(state, session);
                        refresh_view(s);
                        sync_ui_reply(s);
                    }
                    fill_view(&mut reply, s);
                    // 宿主自绘候选时,高亮变化要同步到内联预编辑;否则文档里
                    // 还是旧候选,点选前后看到的文本对不上。
                    reply.preedit = s
                        .engine
                        .composition()
                        .map(|c| c.display.clone())
                        .unwrap_or_default();
                    reply.chinese = s.engine.mode() == Mode::Chinese;
                }
                reply
            }
            Request::Page { session, delta } => {
                state.active.store(session, Ordering::Relaxed);
                if let Some(s) = sessions.get_mut(&session)
                    && s.engine.page_by(delta)
                {
                    // 用户正在翻看候选,别让停顿重排回来把当前页换掉。
                    crate::settle::cancel(state, session);
                    refresh_view(s);
                    sync_ui_reply(s);
                }
                Reply::default()
            }
            Request::UiMode {
                session,
                host_draws,
            } => {
                if let Some(s) = sessions.get_mut(&session) {
                    s.host_draws = host_draws;
                }
                neutral(state)
            }
            Request::ReloadConfig => {
                if let Some(path) = &state.config_path {
                    let cfg = config::load(path);
                    apply_config(state, &mut sessions, cfg);
                }
                Reply::default()
            }
            Request::TogglePunct => {
                let cfg = {
                    let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
                    cfg.chinese_punct = !cfg.chinese_punct;
                    cfg.clone()
                };
                // Ctrl+. 是隐藏快捷键;借模式徽标短暂显示「，。」或「,.」,让用户知道切换结果。
                let label = if cfg.chinese_punct { "，。" } else { ",." };
                if let Some(path) = &state.config_path
                    && let Err(e) = config::save(path, &cfg)
                {
                    println!("保存配置 {} 失败: {e}", path.display());
                }
                apply_config(state, &mut sessions, cfg);
                show_hud(state, &sessions, label, None);
                neutral(state)
            }
            Request::ExportUser => Reply {
                user: state
                    .user
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .export(),
                ..Reply::default()
            },
            Request::ImportUser { text } => {
                state
                    .user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .import(&text);
                if let Some(path) = crate::user_store::path() {
                    crate::user_store::flush(&state.user, &path);
                }
                let cfg = state
                    .config
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                apply_config(state, &mut sessions, cfg);
                group_reply(&state.user)
            }
            Request::ClearUser => {
                state
                    .user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
                // 立即落盘:清空后马上退出也不能让旧词库在下一次启动回来。
                if let Some(path) = crate::user_store::path() {
                    crate::user_store::flush(&state.user, &path);
                }
                // 正在组句的候选可能含刚学过的词;按当前配置重算一遍。
                let cfg = state
                    .config
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                apply_config(state, &mut sessions, cfg);
                Reply::default()
            }
            Request::FuzzyStats => group_reply(&state.user),
            Request::ResetFuzzy => {
                state
                    .user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .reset_groups();
                if let Some(path) = crate::user_store::path() {
                    crate::user_store::flush(&state.user, &path);
                }
                let cfg = state
                    .config
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                apply_config(state, &mut sessions, cfg);
                group_reply(&state.user)
            }
            Request::Shutdown => {
                // 先让管道把应答写完,再由本进程退出;退出前同步落盘。
                if let Some(path) = crate::user_store::path() {
                    crate::user_store::flush(&state.user, &path);
                }
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    std::process::exit(0);
                });
                Reply::default()
            }
            Request::Goodbye { session } => {
                if sessions.remove(&session).is_some() {
                    println!("会话 {session} 下线");
                }
                if state.active.load(Ordering::Relaxed) == session {
                    state.active.store(0, Ordering::Relaxed);
                }
                crate::settle::cancel(state, session);
                Reply::default()
            }
        }
    }

    fn current_mode(state: &State) -> Mode {
        if state.chinese.load(Ordering::Relaxed) {
            Mode::Chinese
        } else {
            Mode::English
        }
    }

    /// 不带文档改动的应答:只同步全局中/英状态,别把 `Reply::default()` 的 false 当英文。
    pub(super) fn neutral(state: &State) -> Reply {
        Reply {
            chinese: state.chinese.load(Ordering::Relaxed),
            fullwidth: state.fullwidth.load(Ordering::Relaxed),
            ..Reply::default()
        }
    }

    /// 当前会话状态原样返回(用于被忽略的 Shift)。
    fn current_reply(s: &Session) -> Reply {
        let mut reply = Reply {
            preedit: s.view.preedit.clone(),
            chinese: s.engine.mode() == Mode::Chinese,
            ..Reply::default()
        };
        fill_view(&mut reply, s);
        reply
    }

    /// 把当前候选页附在应答上;TSF 的候选 UI 元素据此回答宿主和读屏软件。
    fn fill_view(reply: &mut Reply, s: &Session) {
        reply.candidates.clone_from(&s.view.candidates);
        reply.page = s.view.page;
        reply.page_size = s.view.page_size;
        reply.highlight = s.view.highlight;
        reply.fullwidth = s.view.fullwidth;
    }

    fn commits_of(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Commit(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    fn reply_from(s: &Session, events: &[Event]) -> Reply {
        Reply {
            commits: commits_of(events),
            preedit: s
                .engine
                .composition()
                .map(|c| c.display.clone())
                .unwrap_or_default(),
            chinese: s.engine.mode() == Mode::Chinese,
            passthrough: events.iter().any(|e| matches!(e, Event::Passthrough(_))),
            ..Reply::default()
        }
    }

    /// 切模式并把组合里的内容落定。`process(Shift)` 本身会先上屏首选再切。
    fn switch_mode(s: &mut Session, chinese: bool) -> Vec<Event> {
        if (s.engine.mode() == Mode::Chinese) == chinese {
            Vec::new()
        } else {
            s.engine.process(Key::Shift)
        }
    }

    /// 中/英是全局状态:一个应用里切了,所有会话与之后的候选窗都跟着变。
    fn set_global_mode(state: &State, sessions: &mut HashMap<u64, Session>, chinese: bool) {
        let changed = state.chinese.swap(chinese, Ordering::Relaxed) != chinese;
        let caret = if changed {
            let active = state.active.load(Ordering::Relaxed);
            sessions.get(&active).and_then(|s| s.view.caret)
        } else {
            None
        };
        let target = if chinese {
            Mode::Chinese
        } else {
            Mode::English
        };
        for (&session, s) in sessions.iter_mut() {
            if s.engine.mode() != target {
                // 别的窗口可能正组合着。清掉组合前先整串落定,把上屏文本放进
                // ui_reply,由那个窗口自己的轮询写进文档;否则内联拼音会僵住。
                let events = switch_mode(s, chinese);
                let commits = commits_of(&events);
                s.pending = None;
                crate::settle::cancel(state, session);
                refresh_view(s);
                if !commits.is_empty() {
                    let pending = s.ui_reply.get_or_insert_with(Reply::default);
                    pending.commits.extend(commits);
                }
                if let Some(pending) = s.ui_reply.as_mut() {
                    pending.preedit = s.view.preedit.clone();
                    pending.chinese = chinese;
                    pending.fullwidth = s.view.fullwidth;
                }
            } else if s.view.chinese != chinese {
                s.view.chinese = chinese;
            }
        }
        if changed {
            // 只给用户主动切换做徽标;焦点同步、同值 SetMode 不弹。
            show_hud(state, sessions, if chinese { "中" } else { "英" }, caret);
        }
    }

    /// 让模式徽标在光标旁短暂显示一个标记。`caret` 由调用方在会话表还可用时取好。
    fn show_hud(
        state: &State,
        sessions: &HashMap<u64, Session>,
        label: &'static str,
        caret: Option<(i32, i32, i32, i32)>,
    ) {
        let caret = caret.or_else(|| {
            let active = state.active.load(Ordering::Relaxed);
            sessions.get(&active).and_then(|s| s.view.caret)
        });
        *state.mode_hud.lock().unwrap_or_else(|p| p.into_inner()) = Some(ModeHud {
            label,
            caret,
            at: Instant::now(),
        });
        state.hud_serial.fetch_add(1, Ordering::Relaxed);
    }

    /// 全角/半角是全局状态;只改后续输出,不落定已有组合。
    fn set_global_fullwidth(state: &State, sessions: &mut HashMap<u64, Session>, fullwidth: bool) {
        let changed = state.fullwidth.swap(fullwidth, Ordering::Relaxed) != fullwidth;
        for s in sessions.values_mut() {
            s.engine.set_fullwidth(fullwidth);
            s.view.fullwidth = fullwidth;
            // 预演或排队中的应答带着旧全角位;一并改掉,免得下一次写回拨回指示器。
            if let Some(reply) = s.pending.as_mut() {
                reply.fullwidth = fullwidth;
            }
            if let Some(reply) = s.ui_reply.as_mut() {
                reply.fullwidth = fullwidth;
            }
        }
        if changed {
            show_hud(state, sessions, if fullwidth { "全" } else { "半" }, None);
        }
    }

    /// 设置窗读取/重置模糊组学习后的应答。
    fn group_reply(user: &SharedUser) -> Reply {
        Reply {
            groups: user
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .group_stats()
                .into_iter()
                .map(|(name, count)| (name.to_string(), count))
                .collect(),
            ..Reply::default()
        }
    }

    /// 设置重载:写回内存配置,并让所有现有会话按新配置重算。
    fn apply_config(state: &State, sessions: &mut HashMap<u64, Session>, cfg: AppConfig) {
        let engine_cfg = cfg.engine();
        *state.config.write().unwrap_or_else(|p| p.into_inner()) = cfg;
        for s in sessions.values_mut() {
            s.engine.set_config(engine_cfg.clone());
            // 候选重算后旧按键应答的内联文本/候选都不再有效。
            s.pending = None;
            refresh_view(s);
            // 点选待写回的文本保留,但内联文本要跟上新配置。
            if let Some(pending) = s.ui_reply.as_mut() {
                pending.preedit = s.view.preedit.clone();
                pending.chinese = s.view.chinese;
                pending.fullwidth = s.view.fullwidth;
            }
        }
        state.version.fetch_add(1, Ordering::Relaxed);
    }

    /// 应用一条左邻文本;组合已建立时按新上下文重跑同步重排。
    fn apply_context(state: &State, session: u64, s: &mut Session, text: &str) {
        // 组合里已经上屏过分段词时,左邻文本不含这些词,不能用它覆盖。
        if s.engine.composition().is_some() && s.engine.composition_committed() {
            return;
        }
        // 焦点探测已经应用过空上下文时,UIA 的空结果不再触发一次重排。
        if text.is_empty() && s.engine.context().is_empty() {
            return;
        }
        s.engine.set_context(text);
        if s.engine.composition().is_none() {
            return;
        }
        // 用户已经翻页/移动高亮时不再改变候选顺序;仍要撤掉旧停顿任务。
        if s.engine.page() == 0 && s.engine.highlight() == 0 {
            apply_sync_rerank(s);
            refresh_view(s);
            // 在途的按键应答/点选待写回里还是重排前的内联文本。
            if let Some(pending) = s.pending.as_mut() {
                pending.preedit = s.view.preedit.clone();
                pending.candidates = s.view.candidates.clone();
                pending.page = s.view.page;
                pending.page_size = s.view.page_size;
                pending.highlight = s.view.highlight;
            }
            if let Some(pending) = s.ui_reply.as_mut() {
                pending.preedit = s.view.preedit.clone();
                pending.candidates = s.view.candidates.clone();
                pending.page = s.view.page;
                pending.page_size = s.view.page_size;
                pending.highlight = s.view.highlight;
            }
            state.version.fetch_add(1, Ordering::Relaxed);
        }
        crate::settle::schedule(state, session, s);
    }

    /// UIA 线程读完左邻文本后调用;会话、代次或引擎上下文过期就丢弃。
    pub(super) fn apply_uia_context(state: &State, job: &crate::uia::Job, text: &str) {
        if state.active.load(Ordering::Relaxed) != job.session {
            return;
        }
        let mut sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(s) = sessions.get_mut(&job.session) else {
            return;
        };
        if s.context_epoch != job.epoch
            || s.context_seq != job.seq
            || s.engine.context() != job.context.as_str()
        {
            return;
        }
        // UIA 选区可能停在组句文本之后;组句还没上屏,不能混进左邻上下文。
        let mut text = text.to_string();
        let preedit = s.view.preedit.as_str();
        if !preedit.is_empty() && text.ends_with(preedit) {
            text.truncate(text.len() - preedit.len());
        }
        apply_context(state, job.session, s, &text);
    }

    /// 4M 在每个按键返回前跑一次;门控不通过时立即返回,不碰模型。
    fn apply_sync_rerank(s: &mut Session) {
        // 快捷候选固定在本轮插入的位置,4M 模型不能把它们换走。
        if s.engine.candidates_detailed().iter().any(|cand| cand.quick) {
            return;
        }
        let Some(rerank) = s.sync_rerank.as_mut() else {
            return;
        };
        let Some(snap) = crate::settle::snapshot(&s.engine) else {
            return;
        };
        let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
        if let Some(index) = rerank.decide(&snap.context, &texts, &snap.facts) {
            s.engine.promote(index);
        }
    }

    /// 待写回的鼠标点选/跨窗口切模式内容:只更新内联文本与模式,
    /// 已经取到的上屏文本保持不变。
    fn sync_ui_reply(s: &mut Session) {
        if let Some(pending) = s.ui_reply.as_mut() {
            pending.preedit = s.view.preedit.clone();
            pending.chinese = s.view.chinese;
            pending.fullwidth = s.view.fullwidth;
        }
    }

    /// 同步会话视图(候选窗渲染的数据源)。
    pub(super) fn refresh_view(s: &mut Session) {
        if let Some(comp) = s.engine.composition() {
            // 光标可在拼音串中移动;编辑时在光标处插入标记,长串只保留光标附近
            // 一段,确保标记不会被候选窗的 220px 单行截断。光标在末尾时不插
            // 标记,只显示末尾一段。
            let cursor = comp.cursor.min(comp.raw.len());
            let len = comp.raw.len();
            let mut raw = String::new();
            if cursor < len {
                let start = cursor.saturating_sub(12);
                let end = (cursor + 12).min(len);
                if start > 0 {
                    raw.push('…');
                }
                raw.push_str(&comp.raw[start..cursor]);
                raw.push('|');
                raw.push_str(&comp.raw[cursor..end]);
                if end < len {
                    raw.push('…');
                }
            } else if len <= 24 {
                raw.push_str(&comp.raw);
            } else {
                raw.push('…');
                raw.push_str(&comp.raw[len - 24..]);
            }
            s.view.raw = raw;
            s.view.preedit = comp.display.clone();
            s.view.segments = comp
                .seg_spans
                .iter()
                .map(|(a, b)| comp.raw[*a..*b].to_string())
                .collect();
        } else {
            s.view.raw.clear();
            s.view.preedit.clear();
            s.view.segments.clear();
        }
        let (texts, hints): (Vec<String>, Vec<String>) = s
            .engine
            .candidates_detailed()
            .iter()
            .map(|c| (c.text.clone(), c.hint.clone().unwrap_or_default()))
            .unzip();
        s.view.candidates = texts;
        s.view.hints = hints;
        s.view.page = s.engine.page();
        s.view.page_size = s.engine.page_size();
        s.view.highlight = s.engine.highlight();
        s.view.chinese = s.engine.mode() == Mode::Chinese;
    }
}

#[cfg(windows)]
mod pipe;

#[cfg(windows)]
mod settle;

#[cfg(windows)]
mod uia;

#[cfg(windows)]
mod ui;

#[cfg(windows)]
mod user_store;

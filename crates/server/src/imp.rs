//! Windows 宿主进程:启动、共享状态、会话辅助函数。
//!
//! 请求分发在 `request` 子模块;两边共享 `State`/`Session` 与私有辅助函数,
//! 拆分只改变文件边界,不改变可见性和调用路径。

#[path = "server_request.rs"]
mod request;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::Instant;

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_engine::module::Modules;
use lueying_engine::scheme::Scheme;
use lueying_engine::{Action, CivilTime, Engine, Event, Key, Mode, SharedUser, UserDict};
use lueying_features::{CustomPhrases, Prediction, ScriptModule};
use lueying_protocol::{KeyMsg, Reply, Request, SessionView, pipe_name};
use lueying_rerank::{Model, Rerank};
use lueying_server::command_panel::CommandItem;
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
    /// 当前输入范围是密码/隐私字段;运行期脚本整个跳过。
    pub(super) private: bool,
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

/// 动作候选与命令面板执行命令的入口:请求线程投完整命令 id,脚本线程执行。
pub(super) struct ExtHub {
    pub(super) invoke: mpsc::Sender<ExtInvoke>,
    /// 完整命令 id → 标题;扩展重载后由脚本线程刷新。
    pub(super) titles: Arc<Mutex<HashMap<String, String>>>,
}

/// 一次命令执行请求;来源只用于日志。
pub(super) struct ExtInvoke {
    pub(super) command: String,
    pub(super) source: &'static str,
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
    /// 候选窗当前贴光标的上方还是下方;模式徽标据此躲到另一侧。
    pub(super) candidate_above: AtomicBool,
    /// 最近收到按键/光标上报的会话,候选窗只跟它。
    pub(super) active: AtomicU64,
    /// 三档降级后实际生效的主词典来源:`pyd`、`tsv` 或 `builtin`。
    pub(super) dict_source: &'static str,
    pub(super) sync_model: Option<Model>,
    pub(super) settled_model: Option<Model>,
    pub(super) settle_tx: Option<mpsc::Sender<crate::settle::SettleMessage>>,
    /// TSF 读不到左邻文本时交给 UIA 线程补读;容量 1,忙时丢新任务。
    pub(super) uia_tx: Option<mpsc::SyncSender<crate::uia::Job>>,
    /// 所有会话共用一份学习结果。
    pub(super) user: SharedUser,
    /// 对外 commit 通道的令牌缓存;首次使用时从文件读或生成。
    pub(super) commit_token: Mutex<Option<String>>,
    /// 声明式皮肤(skin.toml);重载配置时一并重读。
    pub(super) skin: Mutex<lueying_server::skin::Skin>,
    /// 停顿重排的提供者缝;默认空槽走内置模型。
    pub(super) settled_slot: Mutex<lueying_server::seam::SettledSlot>,
    /// 宿主组合好的可选功能模块;所有会话共享同一组注册表。
    pub(super) modules: Arc<Modules>,
    /// 自定义短语模块;设置重载时重新读文件。
    pub(super) phrases: Option<Arc<CustomPhrases>>,
    /// 下一词联想模块;设置页直接翻它的开关,不必重建注册表。
    pub(super) prediction: Arc<Prediction>,
    /// 简繁转换模块;设置重载时切换目标字形,不必重建注册表。
    pub(super) script: Arc<ScriptModule>,
    /// 当前输入方案;设置切换后新会话与现有会话都跟着换。
    pub(super) scheme: RwLock<Arc<dyn Scheme>>,
    /// 设置窗保存的配置;新会话取它,重载时下发给所有会话。
    pub(super) config: RwLock<AppConfig>,
    /// 动作候选执行命令的通道;脚本线程启动时装填。
    pub(super) ext_hub: Mutex<Option<ExtHub>>,
    /// 命令面板的唤出通道;面板线程启动时装填。
    pub(super) panel: Mutex<Option<mpsc::SyncSender<Vec<CommandItem>>>>,
    /// 面板正开着;此时再按总热键直接丢弃,关闭后不补开。
    pub(super) panel_open: AtomicBool,
    pub(super) config_path: Option<PathBuf>,
    /// 全局中/英模式:所有应用共享,微软拼音同款;新会话上线时跟随。
    pub(super) chinese: AtomicBool,
    /// 全局全角/半角:Shift+Space 或系统转换模式切换;新会话上线时跟随。
    pub(super) fullwidth: AtomicBool,
}

/// 管道与候选窗共用的请求入口;实现放在 `request` 子模块。
pub(crate) fn handle(state: &State, req: Request) -> Reply {
    request::dispatch(state, req)
}

/// 把三个标准句柄接到 NUL。进程被无控制台、无句柄继承的方式拉起时,
/// `GetStdHandle` 返回空句柄,`println!` 写失败会直接 panic。
fn attach_null_stdio() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::System::Console::{
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    let Ok(nul) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")
    else {
        return;
    };
    let handle = HANDLE(nul.as_raw_handle());
    unsafe {
        let _ = SetStdHandle(STD_INPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    // 标准句柄要用到进程结束,句柄不能随局部变量关闭。
    std::mem::forget(nul);
}

/// 标准输出已经被调用方重定向(E2E 把 stdout 收进自己的日志)时不再接管。
fn output_redirected() -> bool {
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // let-else 的判定式不能以块结尾,括号是语法需要的。
    let Ok(handle) = (unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }) else {
        return false;
    };
    !handle.is_invalid() && !handle.0.is_null()
}

/// 把标准输出与错误接到日志文件;超过 1 MB 先滚动一份。
///
/// 服务以「无句柄」方式被拉起时,`println!` 的启动与降级信息原本全进 NUL。
/// 接到文件以后,启动、降级与 panic 才有据可查,设置里也能把日志导出去。
/// 调用方显式接管标准输出时(自动化测试)保持原样;失败时保留
/// [`attach_null_stdio`] 设好的 NUL 句柄。
fn attach_log_stdio() -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

    const LIMIT: u64 = 1024 * 1024;
    let Some(path) = config::log_path() else {
        return false;
    };
    let Some(dir) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LIMIT) {
        let old = path.with_file_name("server.log.1");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&path, &old);
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return false;
    };
    let handle = HANDLE(file.as_raw_handle());
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    // 句柄要用到进程结束,不能随局部变量关闭。
    std::mem::forget(file);
    println!(
        "\n--- lueying-server {} 启动 ---",
        env!("CARGO_PKG_VERSION")
    );
    true
}

/// 服务只把标准输出当日志:先把空句柄接到 NUL,再尽量改接到文件。
fn attach_stdio() {
    if output_redirected() {
        return;
    }
    attach_null_stdio();
    attach_log_stdio();
}

/// 命令面板自检入口:`lueying-server.exe --panel-selftest <报告>`。
/// 独立进程跑,不碰单实例锁、管道与 TSF;结论写文件供 win-e2e 断言。
pub(super) fn panel_selftest() {
    let report = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/panel-selftest.txt"));
    let result = crate::ui_panel::selftest(&report);
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

pub(super) fn real_main() {
    // 安装器/TSF 用「不继承句柄」的方式拉起服务,标准句柄为空;而 println!
    // 在写失败时会 panic。先统一接到 NUL,再尽量接到日志文件。
    attach_stdio();
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
    let loaded = ["main.pyd", "main.tsv"].iter().find_map(|name| {
        dict_path(name)
            .and_then(load_dict)
            .map(|dict| (dict, *name))
    });
    let (dict, dict_source) = match loaded {
        Some((dict, name)) => (dict, if name == "main.pyd" { "pyd" } else { "tsv" }),
        None => {
            println!("主词典缺失或损坏,退回内置迷你词库");
            (Dictionary::builtin(), "builtin")
        }
    };
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
    // 自定义短语是可选用户数据:缺失时写一份带注释的模板,读不到就空表。
    let phrases = config::phrases_path().map(|path| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if !path.exists() {
            let _ = std::fs::write(&path, lueying_features::phrases::TEMPLATE);
        }
        Arc::new(CustomPhrases::load(path))
    });
    let scheme = lueying_features::scheme(app_config.scheme, app_config.aux);
    let prediction = Arc::new(Prediction::with_enabled(app_config.predict));
    let script = Arc::new(ScriptModule::new(app_config.script()));
    let (settle_tx, settle_rx) = mpsc::channel();
    let settle_tx = settled_model.as_ref().map(|_| settle_tx);
    let (uia_tx, uia_rx) = mpsc::sync_channel::<crate::uia::Job>(1);
    let state = Arc::new(State {
        dict: Arc::new(dict),
        dict_source,
        gram,
        sessions: Mutex::new(HashMap::new()),
        conns: Mutex::new(HashMap::new()),
        version: AtomicU64::new(0),
        hud_serial: AtomicU64::new(0),
        mode_hud: Mutex::new(None),
        candidate_above: AtomicBool::new(false),
        active: AtomicU64::new(0),
        sync_model,
        settled_model,
        settle_tx,
        uia_tx: Some(uia_tx),
        user,
        commit_token: Mutex::new(None),
        skin: Mutex::new(load_skin()),
        settled_slot: Mutex::new(lueying_server::seam::SettledSlot::new()),
        modules: crate::modules::registry(
            phrases.clone(),
            Arc::clone(&prediction),
            Arc::clone(&script),
        ),
        phrases,
        prediction,
        script,
        scheme: RwLock::new(scheme),
        config: RwLock::new(app_config),
        ext_hub: Mutex::new(None),
        panel: Mutex::new(None),
        panel_open: AtomicBool::new(false),
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
        // 管道服务在后台线程,主线程只等着;窗口各有各的线程。
        let state = Arc::clone(&state);
        std::thread::spawn(move || crate::pipe::listen(&pipe, state));
    }
    // Skia 渲染管线自检(E2E 打开):建窗、Ganesh-D3D12 出一帧、读回像素,
    // 结论写日志;不依赖管道与真实输入,无 UI 环境也能跑。
    if std::env::var_os("LUEYING_SKIA_SELFTEST").is_some() {
        crate::ui_skia_selftest::selftest();
        crate::ext_selftest::run();
    }
    // V3 帧级基准(E2E 打开):CPU 本进程、GPU 子进程,报告落盘随 artifact 保留。
    if let Some(report) = std::env::var_os("LUEYING_SKIA_BENCH") {
        crate::ui_skia_bench::run(&report.to_string_lossy());
    }
    // 无桌面会话(CI/服务)没有窗口站,建窗会失败;输入法本身只依赖管道。
    if std::env::var_os("LUEYING_SERVER_NO_UI").is_some() {
        // 扩展宿主平时跟着候选窗起;无 UI 时单独开一个(Windows E2E 用),
        // 默认不开,空闲成本仍然为零。
        let _host = std::env::var_os("LUEYING_EXT_HOST")
            .map(|_| crate::ext_host::Script::spawn(Arc::clone(&state)));
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

/// 协议按键转换成引擎按键。转换放在 Server 边界,协议 crate 不依赖引擎,
/// TSF DLL 也就不会因此链接进整个引擎。
fn engine_key(key: KeyMsg) -> Key {
    match key {
        KeyMsg::Letter(c) => Key::Letter(c),
        KeyMsg::Digit(d) => Key::Digit(d),
        KeyMsg::Space => Key::Space,
        KeyMsg::Enter => Key::Enter,
        KeyMsg::Esc => Key::Esc,
        KeyMsg::Backspace => Key::Backspace,
        KeyMsg::Delete => Key::Delete,
        KeyMsg::SegmentBackspace => Key::SegmentBackspace,
        KeyMsg::SegmentDelete => Key::SegmentDelete,
        KeyMsg::Left => Key::Left,
        KeyMsg::Right => Key::Right,
        KeyMsg::SegmentLeft => Key::SegmentLeft,
        KeyMsg::SegmentRight => Key::SegmentRight,
        KeyMsg::Up => Key::Up,
        KeyMsg::Down => Key::Down,
        KeyMsg::Home => Key::Home,
        KeyMsg::End => Key::End,
        KeyMsg::PageUp => Key::PageUp,
        KeyMsg::PageDown => Key::PageDown,
        KeyMsg::DeleteCandidate => Key::DeleteCandidate,
        KeyMsg::Punct(c) => Key::Punct(c),
        KeyMsg::Shift => Key::Shift,
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

/// 读声明式皮肤;文件缺失或坏键回落默认,问题写日志。
fn load_skin() -> lueying_server::skin::Skin {
    let Some(path) = lueying_server::config::skin_path() else {
        return lueying_server::skin::Skin::default();
    };
    let (skin, issues) = lueying_server::skin::Skin::load(&path);
    for issue in issues {
        println!("皮肤: {issue}");
    }
    skin
}

/// 设置重载:写回内存配置,并让所有现有会话按新配置重算。
fn apply_config(state: &State, sessions: &mut HashMap<u64, Session>, cfg: AppConfig) {
    // 皮肤文件跟配置一起重读:改完 skin.toml 一次 reload 生效。
    *state.skin.lock().unwrap_or_else(|p| p.into_inner()) = load_skin();
    // 停顿重排缝:配置点名 bypass 且给了 exe 才装;其余情况都是内置(默认关)。
    let provider = match cfg.settled_rerank_provider.as_deref() {
        Some("bypass") => match cfg.bypass_exe.as_deref().map(str::trim) {
            Some(exe) if !exe.is_empty() => {
                let bypass = lueying_bypass::Bypass::new(lueying_bypass::BypassConfig {
                    exe: PathBuf::from(exe),
                    args: Vec::new(),
                    call_timeout: std::time::Duration::from_secs(1),
                });
                println!("停顿重排:提供者=旁路进程({exe})");
                Some(Arc::new(lueying_server::seam::BypassSettled::new(bypass))
                    as Arc<dyn lueying_server::seam::SettledRerank>)
            }
            _ => {
                println!("停顿重排:提供者=bypass 但没配 bypass_exe,用内置模型");
                None
            }
        },
        _ => None,
    };
    let mut slot = lueying_server::seam::SettledSlot::new();
    slot.set(provider);
    *state.settled_slot.lock().unwrap_or_else(|p| p.into_inner()) = slot;
    let engine_cfg = cfg.engine();
    let old = state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let scheme = (old.scheme != cfg.scheme || old.aux != cfg.aux)
        .then(|| lueying_features::scheme(cfg.scheme, cfg.aux));
    let predict = cfg.predict;
    let script = cfg.script();
    if let Some(scheme) = &scheme {
        *state.scheme.write().unwrap_or_else(|p| p.into_inner()) = Arc::clone(scheme);
    }
    *state.config.write().unwrap_or_else(|p| p.into_inner()) = cfg;
    if let Some(phrases) = &state.phrases {
        phrases.reload();
    }
    state.prediction.set_enabled(predict);
    state.script.set_script(script);
    for s in sessions.values_mut() {
        if let Some(scheme) = &scheme {
            s.engine.set_scheme(Arc::clone(scheme));
        }
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
    // 快捷候选固定在本轮插入的位置,4M 模型不能把它们换走;置顶词同理。
    if s.engine
        .candidates_detailed()
        .iter()
        .any(|cand| cand.quick || cand.pinned)
    {
        return;
    }
    let Some(rerank) = s.sync_rerank.as_mut() else {
        return;
    };
    let Some(snap) = crate::settle::snapshot(&s.engine) else {
        return;
    };
    // 没有左邻上下文时,4M 只能按句子自身打分:共享语料上它的排序比引擎
    // 低 7.6 个百分点,而停顿重排的最终结果与是否先跑 4M 完全相同。这段
    // 同步推理(实测 p95 50–70 ms)只剩延迟,直接跳过,让引擎顺序先上屏。
    if snap.context.is_empty() {
        return;
    }
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

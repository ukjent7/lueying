//! Windows 宿主进程:启动、共享状态、会话辅助函数。
//!
//! 请求分发在 `request` 子模块;两边共享 `State`/`Session` 与私有辅助函数,
//! 拆分只改变文件边界,不改变可见性和调用路径。

#[path = "server_request.rs"]
mod request;

#[path = "server_ext.rs"]
mod ext;

#[path = "server_assets.rs"]
mod assets;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, mpsc};
use std::time::{Duration, Instant};

use crate::context::ContextProvider;
use assets::{dict_path, load_dict, load_model, single_instance};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_engine::module::Modules;
use lueying_engine::scheme::Scheme;
use lueying_engine::{CivilTime, Engine, Event, Key, Mode, SharedUser, UserDict};
use lueying_ext::host::{Done, Provider};
use lueying_features::{CustomPhrases, Prediction, ScriptModule};
use lueying_protocol::{KeyMsg, Reply, Request, SessionView, pipe_name};
use lueying_rerank::{LatencyMetrics, Model, Rerank};
use lueying_server::config::{self, AppConfig};
use lueying_server::ext_link::{ExtLink, Handler, Replier, Request as ExtRequest};
use lueying_server::services::{OutputProvider, PanelProvider};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject,
};
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
    /// 停顿重排算出、等下一次 `Sync` 生效的首选。
    pub(super) settle_pick: Option<crate::settle::SettlePick>,
    /// 候选窗鼠标点选/翻页产生的待写回内容,TSF 的 `Sync` 取走。
    pub(super) ui_reply: Option<Reply>,
    /// 8M 每键重排:后台线程独占推理,前缀缓存随会话存活。
    pub(super) sync_rerank: Option<Arc<Mutex<Rerank>>>,
    /// 25M 停顿重排,异步线程在锁内推理,不占 `sessions` 锁。
    pub(super) settled_rerank: Option<Arc<Mutex<Rerank>>>,
    /// 宿主已接管候选绘制;候选窗让位给宿主/读屏。
    pub(super) host_draws: bool,
    /// 当前输入范围是密码/隐私字段;运行期扩展整个跳过。
    pub(super) private: bool,
    /// 最近一次 Context 的焦点代次;异步 UIA 结果据此丢弃过期文本。
    pub(super) context_epoch: u64,
    /// 每次 Context 自增;同一代次里连开两次组合也能区分旧查询。
    pub(super) context_seq: u64,
}

/// 会话向只读读者发布的状态:候选窗、徽标与扩展宿主只看这里。
#[derive(Clone, PartialEq)]
pub(super) struct Published {
    pub(super) view: SessionView,
    pub(super) private: bool,
    pub(super) host_draws: bool,
    /// 引擎里有组合,不论候选窗是否让位给宿主。
    pub(super) composing: bool,
}

impl Published {
    fn of(s: &Session) -> Self {
        Self {
            view: s.view.clone(),
            private: s.private,
            host_draws: s.host_draws,
            composing: s.engine.composition().is_some(),
        }
    }
}

/// 一个会话:独占锁只护住引擎状态,发布视图走 RwLock,检索与读帧都不排队。
pub(super) struct SessionHandle {
    state: Mutex<Session>,
    published: RwLock<Arc<Published>>,
}

impl SessionHandle {
    fn new(session: Session) -> Arc<Self> {
        let published = Arc::new(Published::of(&session));
        Arc::new(Self {
            state: Mutex::new(session),
            published: RwLock::new(published),
        })
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Session> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 独占会话跑一段逻辑;结束时发布视图,有变化就叫醒候选窗。
    pub(super) fn with<R>(&self, state: &State, f: impl FnOnce(&mut Session) -> R) -> R {
        let mut s = self.lock();
        let out = f(&mut s);
        if self.publish(&s) {
            touch_view(state);
        }
        out
    }

    /// 把会话当前状态发给只读读者;内容没变就返回 false,调用方不必重绘。
    pub(super) fn publish(&self, s: &Session) -> bool {
        let next = Published::of(s);
        let mut current = self.published.write().unwrap_or_else(|p| p.into_inner());
        if **current == next {
            return false;
        }
        *current = Arc::new(next);
        true
    }

    pub(super) fn published(&self) -> Arc<Published> {
        Arc::clone(&self.published.read().unwrap_or_else(|p| p.into_inner()))
    }
}

/// 一次用户主动的模式变化;候选窗同进程的徽标据此短暂显示「中」「英」「全」「半」。
pub(super) struct ModeHud {
    pub(super) label: &'static str,
    /// 切换时的宿主光标矩形(物理像素);没有可用光标时退到主屏。
    pub(super) caret: Option<(i32, i32, i32, i32)>,
    pub(super) at: Instant,
}

/// 自建事件:窗口线程等它休眠,请求线程触发它上岗。
///
/// `HANDLE` 里的裸指针不自动 Send/Sync;而 SetEvent/WaitForSingleObject
/// 都是线程安全的内核调用,句柄值本身跨线程传递没有共享可变状态。
#[derive(Clone, Copy)]
pub(super) struct Wake(HANDLE);

// SAFETY: 只按值传递句柄用于线程安全的内核调用,句柄在进程内不关闭。
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}

impl Wake {
    /// 建一个自动复位事件;建不出来时退化成空句柄,等待改走定时。
    fn create() -> Self {
        Self(unsafe { CreateEventW(None, false, false, None) }.unwrap_or_default())
    }

    /// 触发一次,叫醒等待方。
    fn set(self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }

    /// 等一次唤醒;`timeout` 为空表示一直等到有事件。
    pub(super) fn wait(self, timeout: Option<Duration>) {
        if self.0.is_invalid() {
            // 事件建不出来时退化成定时空转,线程不能变成满核忙等。
            std::thread::sleep(timeout.unwrap_or(Duration::from_millis(15)));
            return;
        }
        let ms = timeout.map_or(INFINITE, |wait| {
            wait.as_millis().min(u128::from(u32::MAX)) as u32
        });
        unsafe {
            let _ = WaitForSingleObject(self.0, ms);
        }
    }

    /// 裸句柄;`MsgWaitForMultipleObjectsEx` 要按数组等。
    pub(super) fn raw(self) -> HANDLE {
        self.0
    }
}

pub(super) struct State {
    pub(super) dict: Arc<Dictionary>,
    pub(super) gram: Option<Arc<Gram>>,
    pub(super) sessions: Mutex<HashMap<u64, Arc<SessionHandle>>>,
    /// 会话 → 当前管道的连接号。宿主崩溃后按连接清理,避免会话与缓存常驻。
    pub(super) conns: Mutex<HashMap<u64, u64>>,
    /// 每次会话变化自增,候选窗据此决定刷新。
    pub(super) version: AtomicU64,
    /// 每次中英切换自增,模式徽标据此决定是否重新显示。
    pub(super) hud_serial: AtomicU64,
    /// 候选窗的空闲等待;版本号变化时叫醒它。
    pub(super) wake_ui: Wake,
    /// 模式徽标的空闲等待;切换与淡入淡出期间叫醒它。
    pub(super) wake_hud: Wake,
    pub(super) mode_hud: Mutex<Option<ModeHud>>,
    /// 候选窗当前贴光标的上方还是下方;模式徽标据此躲到另一侧。
    pub(super) candidate_above: AtomicBool,
    /// 最近收到按键/光标上报的会话,候选窗只跟它。
    pub(super) active: AtomicU64,
    /// 三档降级后实际生效的主词典来源:`pyd`、`tsv` 或 `builtin`。
    pub(super) dict_source: &'static str,
    pub(super) sync_model: Option<Model>,
    pub(super) settled_model: Option<Model>,
    /// 8M 按键重排的作业通道;按键只发快照,推理在后台。
    pub(super) sync_tx: Option<mpsc::SyncSender<crate::sync_rerank::Job>>,
    /// 8M 按键同步重排的延迟分布;所有会话共享一份。
    pub(super) rerank_sync_metrics: Arc<LatencyMetrics>,
    /// 25M 停顿重排的延迟分布;所有会话共享一份。
    pub(super) rerank_settle_metrics: Arc<LatencyMetrics>,
    pub(super) settle_tx: Option<mpsc::Sender<crate::settle::SettleMessage>>,
    /// TSF 读不到左邻文本时交给 UIA 线程补读;容量 1,忙时丢新任务。
    pub(super) uia_tx: Option<mpsc::SyncSender<crate::uia::Job>>,
    /// 所有会话共用一份学习结果。
    pub(super) user: SharedUser,
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
    /// 扩展宿主连接:配置、上下文、外观提交与动作命令都经它转给宿主进程。
    pub(super) ext: Arc<lueying_server::ext_link::ExtLink>,
    /// 扩展 `panel.show` 的宿主选择面板;窗口线程启动时装填。
    pub(super) panel_service: Arc<lueying_server::services::PanelProvider>,
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

impl State {
    /// 按线程号取会话;只取句柄,不碰会话锁。
    pub(super) fn session(&self, id: u64) -> Option<Arc<SessionHandle>> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned()
    }

    /// 独占一个会话跑一段逻辑;结束时把视图发布出去,有变化就叫醒候选窗。
    pub(super) fn with_session<R>(&self, id: u64, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
        Some(self.session(id)?.with(self, f))
    }

    /// 全部会话的句柄快照;遍历时不再持有会话表锁。
    pub(super) fn all_sessions(&self) -> Vec<(u64, Arc<SessionHandle>)> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(&id, handle)| (id, Arc::clone(handle)))
            .collect()
    }

    /// 标记活动会话;换了会话就叫醒候选窗,窗口只跟一个宿主。
    pub(super) fn activate(&self, session: u64) {
        if self.active.swap(session, Ordering::Relaxed) != session {
            touch_view(self);
        }
    }

    /// 当前活动会话的发布视图。
    pub(super) fn active_published(&self) -> Option<Arc<Published>> {
        self.session(self.active.load(Ordering::Relaxed))
            .map(|handle| handle.published())
    }
}

/// 候选窗的数据变了:版本号自增并叫醒等待的窗口线程。
pub(super) fn touch_view(state: &State) {
    state.version.fetch_add(1, Ordering::Release);
    state.wake_ui.set();
    // 输入上下文跟着视图走:隐私门禁与热键的 when 判断在宿主侧用它。
    let active = state.active_published();
    state.ext.set_context(lueying_ext::proto::link::Context {
        private: active.as_ref().is_some_and(|published| published.private),
        editable: active.is_some(),
        composing: active.as_ref().is_some_and(|published| published.composing),
    });
}

/// 只叫醒候选窗,不宣布新视图;脚本补丁走这条轻量路径。
pub(super) fn wake_ui(state: &State) {
    state.wake_ui.set();
}

/// 模式徽标该刷新了。
pub(super) fn touch_hud(state: &State) {
    state.hud_serial.fetch_add(1, Ordering::Release);
    state.wake_hud.set();
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

/// 选择面板自检入口:`lueying-server.exe --panel-selftest <报告>`。
/// 独立进程跑,不碰单实例锁、管道与 TSF;结论写文件供 win-e2e 断言。
pub(super) fn panel_selftest() {
    let report = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/panel-selftest.txt"));
    let result = crate::ui::picker::selftest(&report);
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
    // 语言模型/搭配表是可选数据:缺失或格式不符就退化为纯词频,不影响输入。
    let gram = dict_path("collocation.bin").and_then(|p| match Gram::open(&p) {
        Ok(g) => {
            println!("搭配表/语法模型 ← {}", p.display());
            Some(Arc::new(g))
        }
        Err(e) => {
            println!("搭配表/语法模型 {} 不可用: {e}", p.display());
            None
        }
    });
    // 首次整句解码要算词频总和;放在启动时空转一次,别让首键承担。
    let _ = dict.total_weight();
    // 两档神经重排都是可选数据:缺失/损坏就只保留词频候选。
    let sync_model = load_model("rerank-8m");
    let settled_model = load_model("rerank-25m");
    let settle_available = settled_model.is_some() || sync_model.is_some();
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
    let settle_tx = settle_available.then_some(settle_tx);
    let (sync_tx, sync_rx) = mpsc::sync_channel(1);
    let sync_tx = sync_model.as_ref().map(|_| sync_tx);
    let (uia_tx, uia_rx) = mpsc::sync_channel::<crate::uia::Job>(1);
    // 两个窗口线程各等自己的一份数据;建不出事件时等待退化成定时轮询。
    let wake_ui = Wake::create();
    let wake_hud = Wake::create();
    let panel_service = Arc::new(lueying_server::services::PanelProvider::new());
    let ext = Arc::new(spawn_ext_host(
        Arc::clone(&panel_service),
        Arc::new(crate::context::ContextProvider::new()),
    ));
    ext.set_config(
        app_config.extensions.clone(),
        app_config.bindings.clone(),
        app_config.ocr_backend == config::OcrBackend::Ppocr,
    );
    let state = Arc::new(State {
        dict: Arc::new(dict),
        dict_source,
        gram,
        sessions: Mutex::new(HashMap::new()),
        conns: Mutex::new(HashMap::new()),
        version: AtomicU64::new(0),
        hud_serial: AtomicU64::new(0),
        wake_ui,
        wake_hud,
        mode_hud: Mutex::new(None),
        candidate_above: AtomicBool::new(false),
        active: AtomicU64::new(0),
        sync_model,
        settled_model,
        rerank_sync_metrics: Arc::new(LatencyMetrics::default()),
        rerank_settle_metrics: Arc::new(LatencyMetrics::default()),
        sync_tx,
        settle_tx,
        uia_tx: Some(uia_tx),
        user,
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
        ext: Arc::clone(&ext),
        panel_service,
        config_path,
        chinese: AtomicBool::new(true),
        fullwidth: AtomicBool::new(false),
    });
    {
        // 外观补丁到达:叫醒候选窗重绘。状态对象只弱引用,不随宿主连接成环。
        let weak = Arc::downgrade(&state);
        state.ext.on_patch(move || {
            if let Some(state) = weak.upgrade() {
                state.wake_ui.set();
            }
        });
    }
    if state.settle_tx.is_some() {
        crate::settle::spawn(Arc::clone(&state), settle_rx);
    }
    if state.sync_tx.is_some() {
        crate::sync_rerank::spawn(Arc::clone(&state), sync_rx);
    }
    crate::uia::spawn(Arc::clone(&state), uia_rx);
    let pipe = pipe_name();
    println!("lueying-server 监听 {pipe}");
    {
        // 管道服务在后台线程,主线程只等着;窗口各有各的线程。
        let state = Arc::clone(&state);
        std::thread::spawn(move || crate::pipe::listen(&pipe, state));
    }
    // D2D 渲染管线自检(E2E 打开):WARP 离屏出一帧、读回像素,结论写日志;
    // 不依赖管道与真实输入,无 UI 环境也能跑。
    if std::env::var_os("LUEYING_D2D_SELFTEST").is_some() {
        crate::ui::d2d::selftest::selftest();
    }
    // 无桌面会话(CI/服务)没有窗口站,建窗会失败;输入法本身只依赖管道。
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
    fill_view(&mut reply, &s.view);
    reply
}

/// 把当前候选页附在应答上;TSF 的候选 UI 元素据此回答宿主和读屏软件。
fn fill_view(reply: &mut Reply, view: &SessionView) {
    reply.candidates.clone_from(&view.candidates);
    reply.page = view.page;
    reply.page_size = view.page_size;
    reply.highlight = view.highlight;
    reply.fullwidth = view.fullwidth;
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
fn set_global_mode(state: &State, chinese: bool) {
    let changed = state.chinese.swap(chinese, Ordering::Relaxed) != chinese;
    let caret = if changed {
        state
            .active_published()
            .and_then(|published| published.view.caret)
    } else {
        None
    };
    let target = if chinese {
        Mode::Chinese
    } else {
        Mode::English
    };
    let mut touched = false;
    for (session, handle) in state.all_sessions() {
        let mut s = handle.lock();
        if s.engine.mode() != target {
            // 别的窗口可能正组合着。清掉组合前先整串落定,把上屏文本放进
            // ui_reply,由那个窗口自己的轮询写进文档;否则内联拼音会僵住。
            let events = switch_mode(&mut s, chinese);
            let commits = commits_of(&events);
            s.pending = None;
            crate::settle::cancel(state, session);
            refresh_view(&mut s);
            if !commits.is_empty() {
                let pending = s.ui_reply.get_or_insert_with(Reply::default);
                pending.commits.extend(commits);
            }
            let preedit = s.view.preedit.clone();
            let fullwidth = s.view.fullwidth;
            if let Some(pending) = s.ui_reply.as_mut() {
                pending.preedit = preedit;
                pending.chinese = chinese;
                pending.fullwidth = fullwidth;
            }
        } else if s.view.chinese != chinese {
            s.view.chinese = chinese;
        }
        touched |= handle.publish(&s);
    }
    if touched {
        touch_view(state);
    }
    if changed {
        // 只给用户主动切换做徽标;焦点同步、同值 SetMode 不弹。
        show_hud(state, if chinese { "中" } else { "英" }, caret);
    }
}

/// 让模式徽标在光标旁短暂显示一个标记。
fn show_hud(state: &State, label: &'static str, caret: Option<(i32, i32, i32, i32)>) {
    let caret = caret.or_else(|| state.active_published().and_then(|p| p.view.caret));
    *state.mode_hud.lock().unwrap_or_else(|p| p.into_inner()) = Some(ModeHud {
        label,
        caret,
        at: Instant::now(),
    });
    touch_hud(state);
}

/// 全角/半角是全局状态;只改后续输出,不落定已有组合。
fn set_global_fullwidth(state: &State, fullwidth: bool) {
    let changed = state.fullwidth.swap(fullwidth, Ordering::Relaxed) != fullwidth;
    let mut touched = false;
    for (_, handle) in state.all_sessions() {
        let mut s = handle.lock();
        s.engine.set_fullwidth(fullwidth);
        s.view.fullwidth = fullwidth;
        // 预演或排队中的应答带着旧全角位;一并改掉,免得下一次写回拨回指示器。
        if let Some(reply) = s.pending.as_mut() {
            reply.fullwidth = fullwidth;
        }
        if let Some(reply) = s.ui_reply.as_mut() {
            reply.fullwidth = fullwidth;
        }
        touched |= handle.publish(&s);
    }
    if touched {
        touch_view(state);
    }
    if changed {
        show_hud(state, if fullwidth { "全" } else { "半" }, None);
    }
}

/// 设置窗读取/重置模糊组学习后的应答。
fn group_reply(user: &SharedUser) -> Reply {
    let user = user.read().unwrap_or_else(|p| p.into_inner());
    Reply {
        groups: user
            .group_stats()
            .into_iter()
            .map(|(name, count)| (name.to_string(), count))
            .collect(),
        user_entries: user.len(),
        ..Reply::default()
    }
}

/// 拉起扩展宿主进程(`lueying-ext-host`,与服务端同目录)。
/// 宿主请求的服务只有输出、选择面板与上下文,它们要碰服务端的窗口与会话,留在这里完成。
fn spawn_ext_host(panel: Arc<PanelProvider>, context: Arc<ContextProvider>) -> ExtLink {
    let exe = std::env::current_exe()
        .map(|path| path.with_file_name("lueying-ext-host.exe"))
        .unwrap_or_default();
    let (data, local) = config::ext_host_dirs().unwrap_or_default();
    let log = local.join("logs").join("ext-host.log");
    let args = vec![data.display().to_string(), local.display().to_string()];
    let handler: Handler = Arc::new(move |_id, request, replier| {
        route_request(&panel, &context, request, replier);
    });
    ExtLink::spawn(exe, args, Some(log), handler)
}

/// 宿主请求的服务在服务端完成;结果经 `replier` 异步回送,超时由宿主侧的调用负责。
fn route_request(
    panel: &PanelProvider,
    context: &ContextProvider,
    request: ExtRequest,
    replier: Replier,
) {
    /// 选择面板要等用户操作,应答等待给足时间。
    const REPLY_WAIT: Duration = Duration::from_secs(300);
    let (done, receiver) = Done::channel();
    let started = match request.service.as_str() {
        "output" => OutputProvider.call(&request.caller, &request.method, &request.args, done),
        "panel" => panel.call(&request.caller, &request.method, &request.args, done),
        "context" => context.call(&request.caller, &request.method, &request.args, done),
        other => Err(format!("服务端没有服务 {other:?}")),
    };
    match started {
        Err(error) => replier.send(Err(error)),
        Ok(()) => {
            let _ = std::thread::Builder::new()
                .name("ext-reply".into())
                .spawn(move || {
                    let result = receiver
                        .recv_timeout(REPLY_WAIT)
                        .unwrap_or_else(|_| Err("服务端没有回传结果".to_string()));
                    replier.send(result);
                });
        }
    }
}

/// 设置重载:写回内存配置,并让所有现有会话按新配置重算。
pub(crate) fn apply_config(state: &State, cfg: AppConfig) {
    let engine_cfg = cfg.engine();
    state.ext.set_config(
        cfg.extensions.clone(),
        cfg.bindings.clone(),
        cfg.ocr_backend == config::OcrBackend::Ppocr,
    );
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
    let mut touched = false;
    for (_, handle) in state.all_sessions() {
        let mut s = handle.lock();
        if let Some(scheme) = &scheme {
            s.engine.set_scheme(Arc::clone(scheme));
        }
        s.engine.set_config(engine_cfg.clone());
        // 候选重算后旧按键应答的内联文本/候选都不再有效。
        s.pending = None;
        refresh_view(&mut s);
        // 点选待写回的文本保留,但内联文本要跟上新配置。
        let preedit = s.view.preedit.clone();
        let chinese = s.view.chinese;
        let fullwidth = s.view.fullwidth;
        if let Some(pending) = s.ui_reply.as_mut() {
            pending.preedit = preedit;
            pending.chinese = chinese;
            pending.fullwidth = fullwidth;
        }
        touched |= handle.publish(&s);
    }
    if touched {
        touch_view(state);
    }
}

/// 应用一条左邻文本;组合已建立时排一次同步重排。
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
        crate::sync_rerank::schedule(state, session, s);
        refresh_view(s);
        // 在途的按键应答/点选待写回里还是重排前的内联文本。
        sync_pending(s);
    }
    crate::settle::schedule(state, session, s);
}

/// UIA 线程读完左邻文本后调用;会话、代次或引擎上下文过期就丢弃。
pub(super) fn apply_uia_context(state: &State, job: &crate::uia::Job, text: &str) {
    if state.active.load(Ordering::Relaxed) != job.session {
        return;
    }
    let Some(handle) = state.session(job.session) else {
        return;
    };
    let mut s = handle.lock();
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
    apply_context(state, job.session, &mut s, &text);
    if handle.publish(&s) {
        touch_view(state);
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

/// 后台重排落地后,把在途应答里的候选与内联文本同步到当前引擎状态。
/// 按键应答在重排完成前就已生成,不同步的话 TSF 会写回旧顺序。
pub(super) fn sync_pending(s: &mut Session) {
    if let Some(pending) = s.pending.as_mut() {
        pending.preedit = s.view.preedit.clone();
        pending.candidates.clone_from(&s.view.candidates);
        pending.page = s.view.page;
        pending.page_size = s.view.page_size;
        pending.highlight = s.view.highlight;
    }
    if let Some(pending) = s.ui_reply.as_mut() {
        pending.preedit = s.view.preedit.clone();
        pending.candidates.clone_from(&s.view.candidates);
        pending.page = s.view.page;
        pending.page_size = s.view.page_size;
        pending.highlight = s.view.highlight;
    }
}

/// 同步会话视图(候选窗渲染的数据源)。
pub(super) fn refresh_view(s: &mut Session) {
    lueying_session::refresh(&s.engine, &mut s.view);
}

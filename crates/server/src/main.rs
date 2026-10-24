//! pinyin-server:常驻服务进程。持有词典(一份)与每个宿主线程一个引擎会话,
//! 经命名管道接收 TSF DLL 的按键请求并返回组句/上屏。候选窗 UI 随后接入。

fn main() {
    #[cfg(windows)]
    imp::real_main();
    #[cfg(not(windows))]
    println!("pinyin-server 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, RwLock, mpsc};

    use pinyin_dict::Dictionary;
    use pinyin_dict::gram::Gram;
    use pinyin_engine::{Config, Engine, Event, Key, Mode, SharedUser, UserDict};
    use pinyin_protocol::{Reply, Request, SessionView, pipe_name};
    use pinyin_rerank::{Model, Rerank};

    pub(super) struct Session {
        pub(super) engine: Engine,
        pub(super) view: SessionView,
        /// `Key` 的预演结果,`TakeKey` 取走(DLL 在 OnKeyDown 应用到文档)。
        pub(super) pending: Option<Reply>,
        /// 4M 每键同步重排,前缀缓存随会话存活。
        pub(super) sync_rerank: Option<Rerank>,
        /// 25M 停顿重排,异步线程在锁内推理,不占 `sessions` 锁。
        pub(super) settled_rerank: Option<Arc<Mutex<Rerank>>>,
    }

    pub(super) struct State {
        pub(super) dict: Arc<Dictionary>,
        pub(super) gram: Option<Arc<Gram>>,
        pub(super) sessions: Mutex<HashMap<u64, Session>>,
        /// 每次会话变化自增,候选窗轮询它决定刷新。
        pub(super) version: AtomicU64,
        /// 最近收到按键/光标上报的会话,候选窗只跟它。
        pub(super) active: AtomicU64,
        pub(super) sync_model: Option<Model>,
        pub(super) settled_model: Option<Model>,
        pub(super) settle_tx: Option<mpsc::Sender<crate::settle::SettleMessage>>,
        /// 所有会话共用一份学习结果。
        pub(super) user: SharedUser,
    }

    pub(super) fn real_main() {
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
        let (settle_tx, settle_rx) = mpsc::channel();
        let settle_tx = settled_model.as_ref().map(|_| settle_tx);
        let state = Arc::new(State {
            dict: Arc::new(dict),
            gram,
            sessions: Mutex::new(HashMap::new()),
            version: AtomicU64::new(0),
            active: AtomicU64::new(0),
            sync_model,
            settled_model,
            settle_tx,
            user,
        });
        if state.settle_tx.is_some() {
            crate::settle::spawn(Arc::clone(&state), settle_rx);
        }
        let pipe = pipe_name();
        println!("pinyin-server 监听 {pipe}");
        {
            // 管道服务在后台线程,主线程留给 gpui 事件循环。
            let state = Arc::clone(&state);
            std::thread::spawn(move || crate::pipe::listen(&pipe, state));
        }
        crate::ui::run(state);
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
                sessions.entry(session).or_insert_with(|| Session {
                    engine: {
                        let engine = Engine::new(Arc::clone(&state.dict), Config::default())
                            .with_user(Arc::clone(&state.user));
                        match &state.gram {
                            Some(g) => engine.with_gram(Arc::clone(g)),
                            None => engine,
                        }
                    },
                    view: SessionView {
                        host,
                        ..SessionView::default()
                    },
                    pending: None,
                    sync_rerank: state.sync_model.as_ref().map(Rerank::new),
                    settled_rerank: state
                        .settled_model
                        .as_ref()
                        .map(|m| Arc::new(Mutex::new(Rerank::new(m)))),
                });
                state.active.store(session, Ordering::Relaxed);
                Reply::default()
            }
            Request::Key { session, key } => {
                state.active.store(session, Ordering::Relaxed);
                let Some(s) = sessions.get_mut(&session) else {
                    return Reply::default();
                };
                let before_rev = s.engine.revision();
                let events = s.engine.process(key.into());
                // 只有候选集合重算过才重排;翻页/移动高亮不能把用户的选择重置。
                if s.engine.revision() != before_rev {
                    apply_sync_rerank(s);
                }
                let passthrough = events.iter().any(|e| matches!(e, Event::Passthrough(_)));
                let reply = Reply {
                    commits: events
                        .iter()
                        .filter_map(|e| match e {
                            Event::Commit(t) => Some(t.clone()),
                            _ => None,
                        })
                        .collect(),
                    preedit: s
                        .engine
                        .composition()
                        .map(|c| c.display.clone())
                        .unwrap_or_default(),
                    chinese: s.engine.mode() == Mode::Chinese,
                    passthrough,
                };
                refresh_view(s);
                s.pending = Some(reply.clone());
                crate::settle::schedule(state, session, s);
                reply
            }
            Request::Sync { session } => sessions
                .get(&session)
                .map(|s| Reply {
                    preedit: s.view.preedit.clone(),
                    chinese: s.view.chinese,
                    ..Reply::default()
                })
                .unwrap_or_default(),
            Request::Context { session, text } => {
                if let Some(s) = sessions.get_mut(&session)
                    // 组合里已经上屏过分段词时,左邻文本不含这些词,不能用它覆盖。
                    && (s.engine.composition().is_none() || !s.engine.composition_committed())
                {
                    s.engine.set_context(&text);
                    // 组合已建立时,首个按键排的任务可能带的是空上下文;重排一次。
                    if s.engine.composition().is_some() {
                        crate::settle::schedule(state, session, s);
                    }
                }
                Reply::default()
            }
            Request::TakeKey { session } => sessions
                .get_mut(&session)
                .and_then(|s| s.pending.take())
                .unwrap_or_default(),
            Request::SetMode { session, chinese } => {
                state.active.store(session, Ordering::Relaxed);
                if let Some(s) = sessions.get_mut(&session) {
                    s.engine.set_mode(if chinese {
                        Mode::Chinese
                    } else {
                        Mode::English
                    });
                    if !chinese {
                        crate::settle::cancel(state, session);
                    }
                    refresh_view(s);
                }
                Reply::default()
            }
            Request::Focus { session, focused } => {
                state.active.store(session, Ordering::Relaxed);
                if let Some(s) = sessions.get_mut(&session) {
                    if !focused {
                        // 失焦:清掉没上屏的拼音和上一窗口的上下文。
                        let _ = s.engine.process(Key::Esc);
                        s.engine.clear_context();
                        crate::settle::cancel(state, session);
                    }
                    refresh_view(s);
                }
                Reply::default()
            }
            Request::CompositionTerminated { session } => {
                if let Some(s) = sessions.get_mut(&session) {
                    let _ = s.engine.process(Key::Esc);
                    crate::settle::cancel(state, session);
                    refresh_view(s);
                }
                Reply::default()
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
            Request::Goodbye { session } => {
                println!("会话 {session} 下线");
                sessions.remove(&session);
                crate::settle::cancel(state, session);
                Reply::default()
            }
        }
    }

    /// 4M 在每个按键返回前跑一次;门控不通过时立即返回,不碰模型。
    fn apply_sync_rerank(s: &mut Session) {
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

    /// 同步会话视图(候选窗渲染的数据源)。
    pub(super) fn refresh_view(s: &mut Session) {
        if let Some(comp) = s.engine.composition() {
            s.view.preedit = comp.display.clone();
            s.view.segments = comp
                .seg_spans
                .iter()
                .map(|(a, b)| comp.raw[*a..*b].to_string())
                .collect();
        } else {
            s.view.preedit.clear();
            s.view.segments.clear();
        }
        s.view.candidates = s.engine.candidates();
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
mod ui;

#[cfg(windows)]
mod user_store;

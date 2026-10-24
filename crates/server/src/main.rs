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
    use std::sync::{Arc, Mutex};

    use pinyin_dict::Dictionary;
    use pinyin_engine::{Config, Engine, Event, Key, Mode};
    use pinyin_protocol::{Reply, Request, SessionView, pipe_name};

    pub(super) struct Session {
        pub(super) engine: Engine,
        pub(super) view: SessionView,
        /// `Key` 的预演结果,`TakeKey` 取走(DLL 在 OnKeyDown 应用到文档)。
        pub(super) pending: Option<Reply>,
    }

    pub(super) struct State {
        pub(super) dict: Arc<Dictionary>,
        pub(super) sessions: Mutex<HashMap<u64, Session>>,
        /// 每次会话变化自增,候选窗轮询它决定刷新。
        pub(super) version: AtomicU64,
        /// 最近收到按键/光标上报的会话,候选窗只跟它。
        pub(super) active: AtomicU64,
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
        let state = Arc::new(State {
            dict: Arc::new(dict),
            sessions: Mutex::new(HashMap::new()),
            version: AtomicU64::new(0),
            active: AtomicU64::new(0),
        });
        let pipe = pipe_name();
        println!("pinyin-server 监听 {pipe}");
        {
            // 管道服务在后台线程,主线程留给 gpui 事件循环。
            let state = Arc::clone(&state);
            std::thread::spawn(move || crate::pipe::listen(&pipe, state));
        }
        crate::ui::run(state);
    }

    /// exe 同目录的 main.pyd / main.tsv。
    fn dict_path(name: &str) -> Option<std::path::PathBuf> {
        let mut path = std::env::current_exe().ok()?;
        path.set_file_name(name);
        path.exists().then_some(path)
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
        // 任何请求后候选窗都可能要重摆,先记账再处理。
        state.version.fetch_add(1, Ordering::Relaxed);
        let mut sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
        match req {
            Request::Hello { session, host } => {
                println!("会话 {session} 上线({host})");
                sessions.entry(session).or_insert_with(|| Session {
                    engine: Engine::new(Arc::clone(&state.dict), Config::default()),
                    view: SessionView {
                        host,
                        ..SessionView::default()
                    },
                    pending: None,
                });
                state.active.store(session, Ordering::Relaxed);
                Reply::default()
            }
            Request::Key { session, key } => {
                state.active.store(session, Ordering::Relaxed);
                let Some(s) = sessions.get_mut(&session) else {
                    return Reply::default();
                };
                let events = s.engine.process(key.into());
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
                reply
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
                    refresh_view(s);
                }
                Reply::default()
            }
            Request::Focus { session, focused } => {
                state.active.store(session, Ordering::Relaxed);
                if let Some(s) = sessions.get_mut(&session) {
                    if !focused {
                        // 失焦:清掉没上屏的拼音。
                        let _ = s.engine.process(Key::Esc);
                    }
                    refresh_view(s);
                }
                Reply::default()
            }
            Request::CompositionTerminated { session } => {
                if let Some(s) = sessions.get_mut(&session) {
                    let _ = s.engine.process(Key::Esc);
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
                }
                Reply::default()
            }
            Request::Goodbye { session } => {
                println!("会话 {session} 下线");
                sessions.remove(&session);
                Reply::default()
            }
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
mod ui;

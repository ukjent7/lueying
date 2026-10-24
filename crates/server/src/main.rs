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
    use std::sync::{Arc, Mutex};

    use pinyin_dict::Dictionary;
    use pinyin_engine::{Config, Engine, Event, Key, Mode};
    use pinyin_protocol::{Reply, Request, SessionView, PIPE_NAME};

    pub(super) struct Session {
        pub(super) engine: Engine,
        pub(super) view: SessionView,
        /// `Key` 的预演结果,`TakeKey` 取走(DLL 在 OnKeyDown 应用到文档)。
        pub(super) pending: Option<Reply>,
    }

    pub(super) struct State {
        pub(super) dict: Arc<Dictionary>,
        pub(super) sessions: Mutex<HashMap<u64, Session>>,
    }

    pub(super) fn real_main() {
        let dict = dict_path()
            .and_then(|p| Dictionary::from_path(&p).ok())
            .map(|(d, bad)| {
                println!("词典 {} 条(跳过坏行 {bad})", d.len());
                d
            })
            .unwrap_or_else(|| {
                println!("主词典缺失,退回内置迷你词库");
                Dictionary::builtin()
            });
        let state = Arc::new(State {
            dict: Arc::new(dict),
            sessions: Mutex::new(HashMap::new()),
        });
        println!("pinyin-server 监听 {PIPE_NAME}");
        crate::pipe::listen(PIPE_NAME, state);
    }

    /// exe 同目录的 main.tsv。
    fn dict_path() -> Option<std::path::PathBuf> {
        let mut path = std::env::current_exe().ok()?;
        path.set_file_name("main.tsv");
        Some(path)
    }

    /// 处理一条请求,返回应答。
    pub(super) fn handle(state: &State, req: Request) -> Reply {
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
                Reply::default()
            }
            Request::Key { session, key } => {
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
            Request::Focus { session, focused } => {
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

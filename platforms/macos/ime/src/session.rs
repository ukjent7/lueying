//! 一个输入会话:引擎、用户词库、两档神经重排与候选快照。
//!
//! 输入法进程常驻,每个客户端连接由 IMKServer 建一个控制器;会话状态
//! 跟着控制器走。所有调用都在主线程,不需要锁。词典与模型进程内共享,
//! 每个连接只建自己的引擎与重排缓存。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_engine::{Config, Engine, Event, Key, Mode, SharedUser, UserDict};
use lueying_protocol::SessionView;
use lueying_rerank::{Fact, Model, Rerank};
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;

pub struct Session {
    pub engine: Engine,
    pub view: SessionView,
    pub client: Option<Retained<AnyObject>>,
    user: SharedUser,
    user_path: Option<PathBuf>,
    sync_rerank: Option<Rerank>,
    settle_rerank: Option<Rerank>,
}

impl Session {
    pub fn new() -> Self {
        let dictionary = shared_dictionary();
        let mut engine = Engine::new(dictionary, Config::default());
        if let Some(gram) = shared_gram() {
            engine = engine.with_gram(gram);
        }
        let user_path = user_path();
        let user: SharedUser = Arc::new(RwLock::new(load_user(user_path.as_deref())));
        engine = engine.with_user(Arc::clone(&user));
        let sync_rerank = shared_rerank(SyncModel::Four).map(|model| Rerank::new(&model));
        let settle_rerank = shared_rerank(SyncModel::TwentyFive).map(|model| Rerank::new(&model));
        Self {
            engine,
            view: SessionView::default(),
            client: None,
            user,
            user_path,
            sync_rerank,
            settle_rerank,
        }
    }

    pub fn set_client(&mut self, client: Option<&AnyObject>) {
        self.client = client.map(|client| client.retain());
    }

    /// 处理一个按键;返回 false 表示交还宿主。
    pub fn process_key(&mut self, key: Key) -> bool {
        let events = self.engine.process(key);
        let passthrough = events
            .iter()
            .any(|event| matches!(event, Event::Passthrough(_)));
        self.apply(events);
        !passthrough
    }

    /// 原样落定当前组合(回车/焦点移走)。
    pub fn commit(&mut self) {
        let text = self.engine.finalize_display().unwrap_or_default();
        if !text.is_empty() {
            self.insert(&text);
        }
        self.clear_marked();
        self.refresh();
    }

    pub fn pick(&mut self, slot: usize) {
        let index = self.engine.page() * self.engine.page_size().max(1) + slot;
        let events = self.engine.pick(index);
        self.apply(events);
    }

    pub fn page(&mut self, delta: i32) {
        if self.engine.page_by(delta) {
            self.refresh();
            self.sync_marked();
        }
    }

    /// 停顿重排:在最近的停顿窗口内跑 25M,结果直接改写引擎顺序。
    pub fn settle(&mut self) {
        let Some(rerank) = self.settle_rerank.as_mut() else {
            return;
        };
        let Some((context, texts, facts, fuzzy)) = snapshot(&self.engine) else {
            return;
        };
        let texts_ref: Vec<&str> = texts.iter().map(String::as_str).collect();
        if let Some(index) = rerank.decide_settled(&context, &texts_ref, &facts, &fuzzy)
            && self.engine.promote(index)
        {
            self.refresh();
            self.sync_marked();
        }
    }

    pub fn refresh(&mut self) {
        if let Some(comp) = self.engine.composition() {
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
            self.view.raw = raw;
            self.view.preedit = comp.display.clone();
            self.view.segments = comp
                .seg_spans
                .iter()
                .map(|(start, end)| comp.raw[*start..*end].to_string())
                .collect();
        } else {
            self.view.raw.clear();
            self.view.preedit.clear();
            self.view.segments.clear();
        }
        let (texts, hints): (Vec<String>, Vec<String>) = self
            .engine
            .candidates_detailed()
            .iter()
            .map(|candidate| (candidate.text.clone(), candidate.hint.clone().unwrap_or_default()))
            .unzip();
        self.view.candidates = texts;
        self.view.hints = hints;
        self.view.page = self.engine.page();
        self.view.page_size = self.engine.page_size();
        self.view.highlight = self.engine.highlight();
        self.view.chinese = self.engine.mode() == Mode::Chinese;
        self.view.fullwidth = self.engine.fullwidth();
    }

    /// 保存用户词库;会话关闭或进程退出时调用。
    pub fn save(&self) {
        let Some(path) = self.user_path.as_deref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = self
            .user
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .export();
        let _ = std::fs::write(path, text);
    }

    fn apply(&mut self, events: Vec<Event>) {
        for event in &events {
            if let Event::Commit(text) = event {
                self.insert(text);
            }
        }
        self.refresh();
        self.sync_marked();
        self.sync_rerank();
    }

    fn sync_rerank(&mut self) {
        let Some(rerank) = self.sync_rerank.as_mut() else {
            return;
        };
        let Some((context, texts, facts, _)) = snapshot(&self.engine) else {
            return;
        };
        // 没有左邻上下文时 4M 的排序弱于引擎原序,直接跳过。
        if context.is_empty() {
            return;
        }
        let texts_ref: Vec<&str> = texts.iter().map(String::as_str).collect();
        if let Some(index) = rerank.decide(&context, &texts_ref, &facts)
            && self.engine.promote(index)
        {
            self.refresh();
            self.sync_marked();
        }
    }

    fn insert(&self, text: &str) {
        if let Some(client) = &self.client {
            crate::controller::insert_text(client, text);
        }
    }

    fn sync_marked(&self) {
        let Some(client) = &self.client else {
            return;
        };
        match self.engine.composition() {
            Some(comp) => crate::controller::set_marked_text(client, &comp.display),
            None => crate::controller::clear_marked_text(client),
        }
    }

    fn clear_marked(&self) {
        if let Some(client) = &self.client {
            crate::controller::clear_marked_text(client);
        }
    }
}

fn snapshot(engine: &Engine) -> Option<(String, Vec<String>, Vec<Fact>, Vec<bool>)> {
    engine.composition()?;
    let candidates = engine.candidates_detailed();
    Some((
        engine.context().to_string(),
        candidates.iter().map(|c| c.text.clone()).collect(),
        candidates
            .iter()
            .map(|c| Fact {
                answers_key: c.full,
                trusted_dictionary_hit: c.trusted,
            })
            .collect(),
        candidates.iter().map(|c| c.edits > 0).collect(),
    ))
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("LUEYING_DATA_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.join("../Resources/data"));
        dirs.push(parent.join("data"));
    }
    dirs.push(PathBuf::from("data"));
    dirs
}

fn shared_dictionary() -> Arc<Dictionary> {
    static DICTIONARY: OnceLock<Arc<Dictionary>> = OnceLock::new();
    Arc::clone(DICTIONARY.get_or_init(|| Arc::new(load_dictionary())))
}

fn load_dictionary() -> Dictionary {
    for dir in data_dirs() {
        let pyd = dir.join("main.pyd");
        if let Ok(bytes) = std::fs::read(&pyd)
            && let Some(dictionary) = Dictionary::from_binary(&bytes)
        {
            return dictionary;
        }
        let tsv = dir.join("main.tsv");
        if let Ok((dictionary, _)) = Dictionary::from_path(&tsv) {
            return dictionary;
        }
    }
    Dictionary::builtin()
}

fn shared_gram() -> Option<Arc<Gram>> {
    static GRAM: OnceLock<Option<Arc<Gram>>> = OnceLock::new();
    GRAM.get_or_init(|| {
        data_dirs()
            .into_iter()
            .find_map(|dir| Gram::open(&dir.join("main.gram")).ok())
            .map(Arc::new)
    })
    .clone()
}

enum SyncModel {
    Four,
    TwentyFive,
}

fn shared_rerank(model: SyncModel) -> Option<Model> {
    static FOUR: OnceLock<Option<Model>> = OnceLock::new();
    static TWENTY_FIVE: OnceLock<Option<Model>> = OnceLock::new();
    let (slot, name) = match model {
        SyncModel::Four => (&FOUR, "rerank-4m.safetensors"),
        SyncModel::TwentyFive => (&TWENTY_FIVE, "rerank-25m.safetensors"),
    };
    slot.get_or_init(|| load_rerank(name)).clone()
}

fn load_rerank(name: &str) -> Option<Model> {
    let bytes = data_dirs()
        .into_iter()
        .find_map(|dir| std::fs::read(dir.join(name)).ok())?;
    Model::load(&bytes).ok()
}

fn user_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/lueying")
            .join("user.tsv"),
    )
}

fn load_user(path: Option<&std::path::Path>) -> UserDict {
    let mut user = UserDict::default();
    if let Some(path) = path
        && let Ok(text) = std::fs::read_to_string(path)
    {
        user.import(&text);
    }
    user
}

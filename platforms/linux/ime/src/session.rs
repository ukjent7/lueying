//! 一个输入会话:引擎、用户词库、两档神经重排与候选快照。
//!
//! Wayland 输入法对象在进程内常驻,同一时间只服务一个文本输入;焦点切换
//! 时清空组合与左邻上下文。所有调用都在 Wayland 事件循环线程,不需要锁。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use lueying_engine::{Engine, Event, Key, Mode, Script, SharedUser, UserDict};
use lueying_features::{CustomPhrases, Prediction, ScriptModule};
use lueying_protocol::SessionView;
use lueying_render::toolbar::Status;
use lueying_rerank::{Model, Rerank};
use lueying_server::config::AppConfig;

pub struct Session {
    pub engine: Engine,
    pub view: SessionView,
    /// 简繁模块的句柄:工具栏的「简/繁」改它,下一次候选构建生效。
    script: Arc<ScriptModule>,
    /// 当前是否繁体输出。
    traditional: bool,
    user: SharedUser,
    user_path: Option<PathBuf>,
    sync_rerank: Option<Mutex<Rerank>>,
    settle_rerank: Option<Mutex<Rerank>>,
}

impl Session {
    pub fn new(app: &AppConfig) -> Self {
        let mut engine = Engine::new(crate::model::shared_dictionary(), app.engine());
        if let Some(gram) = crate::model::shared_gram() {
            engine = engine.with_gram(gram);
        }
        let user_path = crate::config::user_path();
        let user: SharedUser = Arc::new(RwLock::new(load_user(user_path.as_deref())));
        engine = engine.with_user(Arc::clone(&user));
        let phrases_path = phrases_path();
        let phrases = phrases_path
            .as_ref()
            .map(|path| Arc::new(CustomPhrases::load(path.clone())));
        let prediction = Arc::new(Prediction::with_enabled(app.predict));
        let script = Arc::new(ScriptModule::new(app.script()));
        engine = engine
            .with_modules(lueying_features::registry_with_script(
                phrases,
                prediction,
                Arc::clone(&script),
            ))
            .with_scheme(lueying_features::scheme(app.scheme, app.aux));
        engine.set_host("lueying-linux");
        let sync_model = model("rerank-8m");
        let settle_model = model("rerank-25m").or_else(|| sync_model.clone());
        let sync_rerank = sync_model.map(|m| Mutex::new(Rerank::new(&m)));
        let settle_rerank = settle_model.map(|m| Mutex::new(Rerank::new(&m)));
        let traditional = app.script() == Script::Traditional;
        Self {
            engine,
            view: SessionView::default(),
            script,
            traditional,
            user,
            user_path,
            sync_rerank,
            settle_rerank,
        }
    }

    /// 处理一个按键并返回引擎事件;调用方负责把 `Commit` 发给客户端。
    pub fn process_key(&mut self, key: Key) -> Vec<Event> {
        let events = self.engine.process(key);
        self.refresh();
        self.sync_rerank();
        events
    }

    /// 原样落定当前组合(回车、翻页结束或焦点移走)。
    pub fn commit(&mut self) -> Option<String> {
        let text = self.engine.finalize_display();
        self.refresh();
        text.filter(|text| !text.is_empty())
    }

    /// 点选页内候选;返回上屏文本。
    pub fn pick(&mut self, slot: usize) -> Option<String> {
        let index = self.engine.page() * self.engine.page_size().max(1) + slot;
        let events = self.engine.pick(index);
        self.refresh();
        self.sync_rerank();
        commit_text(events)
    }

    pub fn page(&mut self, delta: i32) -> bool {
        let changed = self.engine.page_by(delta);
        if changed {
            self.refresh();
        }
        changed
    }

    /// 停顿重排:在最近的停顿窗口内跑 25M,结果直接改写引擎顺序。
    pub fn settle(&mut self) {
        let Some(rerank) = self.settle_rerank.as_ref() else {
            return;
        };
        let Some(snap) = lueying_session::snapshot(&self.engine) else {
            return;
        };
        if let Some(index) = lueying_session::decide_settled(rerank, &snap)
            && self.engine.promote(index)
        {
            self.refresh();
        }
    }

    pub fn refresh(&mut self) {
        lueying_session::refresh(&self.engine, &mut self.view);
    }

    /// 焦点切换:丢弃组合,清空左邻上下文;保留中英文模式。
    pub fn deactivate(&mut self) {
        if self.engine.composition().is_some() {
            let _ = self.engine.process(Key::Esc);
        }
        self.engine.clear_context();
        self.refresh();
    }

    pub fn set_context(&mut self, text: &str, cursor: u32) {
        let end = floor_char_boundary(text, cursor as usize);
        self.engine.set_context(&text[..end]);
    }

    /// 工具栏按钮上的字要随的状态。
    pub fn status(&self) -> Status {
        Status {
            chinese: self.engine.mode() == Mode::Chinese,
            fullwidth: self.engine.fullwidth(),
            punct: self.engine.config().chinese_punct,
            traditional: self.traditional,
        }
    }

    /// 工具栏的「全/半」:只改后续按键的输出。
    pub fn toggle_fullwidth(&mut self) {
        let on = !self.engine.fullwidth();
        self.engine.set_fullwidth(on);
    }

    /// 工具栏的「，/,」:切换中文标点,立刻按新配置重算。
    pub fn toggle_punct(&mut self) {
        let mut cfg = self.engine.config().clone();
        cfg.chinese_punct = !cfg.chinese_punct;
        self.engine.set_config(cfg);
        self.refresh();
    }

    /// 工具栏的「简/繁」。
    pub fn toggle_script(&mut self) {
        self.traditional = !self.traditional;
        let script = if self.traditional {
            Script::Traditional
        } else {
            Script::Simplified
        };
        self.script.set_script(script);
        self.refresh();
    }

    pub fn set_learning(&mut self, enabled: bool) {
        self.engine.set_learning(enabled);
    }

    pub fn set_now(&mut self) {
        self.engine.set_now(local_time());
    }

    /// 保存用户词库;焦点移走与退出时调用。
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

    fn sync_rerank(&mut self) {
        let Some(rerank) = self.sync_rerank.as_ref() else {
            return;
        };
        let Some(snap) = lueying_session::snapshot(&self.engine) else {
            return;
        };
        if !lueying_session::sync_worthwhile(&snap) {
            return;
        }
        if lueying_session::pinned_candidates(&self.engine) {
            return;
        }
        if let Some(index) = lueying_session::decide_sync(rerank, &snap)
            && self.engine.promote(index)
        {
            self.refresh();
        }
    }
}

/// 会话建立时确保短语模板存在,方便用户直接编辑。
fn phrases_path() -> Option<PathBuf> {
    let path = lueying_server::config::phrases_path()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !path.exists() {
        let _ = std::fs::write(&path, lueying_features::phrases::TEMPLATE);
    }
    Some(path)
}

fn model(stem: &str) -> Option<Model> {
    static EIGHT: OnceLock<Option<Model>> = OnceLock::new();
    static TWENTY_FIVE: OnceLock<Option<Model>> = OnceLock::new();
    let (slot, stem) = match stem {
        "rerank-8m" => (&EIGHT, stem),
        _ => (&TWENTY_FIVE, stem),
    };
    slot.get_or_init(|| crate::model::rerank(stem)).clone()
}

fn commit_text(events: Vec<Event>) -> Option<String> {
    let mut text = String::new();
    for event in events {
        if let Event::Commit(part) = event {
            text.push_str(&part);
        }
    }
    (!text.is_empty()).then_some(text)
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

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn local_time() -> lueying_engine::CivilTime {
    // SAFETY:localtime_r 只写调用方提供的 tm,time 的返回值由调用方持有。
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        lueying_engine::CivilTime {
            year: tm.tm_year + 1900,
            month: tm.tm_mon as u32 + 1,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            weekday: ((tm.tm_wday + 6) % 7 + 1) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::floor_char_boundary;

    #[test]
    fn cursor_never_splits_a_character() {
        let text = "你好abc";
        assert_eq!(floor_char_boundary(text, 0), 0);
        assert_eq!(floor_char_boundary(text, 1), 0);
        assert_eq!(floor_char_boundary(text, 3), 3);
        assert_eq!(floor_char_boundary(text, 99), text.len());
    }
}

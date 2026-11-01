//! 场景层宿主:加载 `scene.toml`、热重载、把会话差异翻成事件、按帧喂信号。
//!
//! 场景运行时(`lueying_scene`)是纯确定性的,素材与文件都在这侧:
//! 目录 `%APPDATA%\lueying\scene`,改完 scene.toml 一秒内生效;解析失败
//! 只废掉场景,候选窗照常。隐私字段(密码框)下整个场景层停用,由窗口层
//! 保证;这里不碰任何候选文字,信号全是几何与计数。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use lueying_protocol::SessionView;
use lueying_scene::{EventName, Inputs, Runtime};

use lueying_server::config;

/// 效果目录的检查间隔;用户改完 scene.toml 一秒内生效。
const RELOAD_CHECK: Duration = Duration::from_millis(1000);
/// 打字速率的统计窗;窗口内次数除以窗长。
const RATE_WINDOW: Duration = Duration::from_secs(2);
/// 退格连发的判定:窗口内缩字到第 3 次触发。
const BURST_COUNT: u32 = 3;
const BURST_WINDOW: Duration = Duration::from_millis(1000);

/// 场景层宿主;归窗口线程所有。
pub(super) struct SceneHost {
    dir: PathBuf,
    runtime: Option<Runtime>,
    stamp: Option<SystemTime>,
    next_check: Instant,
    /// 场景时钟;随每帧推进,「减少动态效果」时冻结。
    time: f64,
    /// 最近击键时刻;打字速率与空闲时长从这里算。
    keys: VecDeque<Instant>,
    /// 上一帧会话视图;事件从相邻两帧的差异里翻出来。
    last: Option<SessionView>,
    burst: u32,
    burst_until: Option<Instant>,
}

impl SceneHost {
    /// 读场景目录;文件不存在或解析失败都不算错误,候选窗照常。
    pub(super) fn new() -> Self {
        let dir = config::scene_dir().unwrap_or_else(|| PathBuf::from("lueying-scene"));
        let mut host = Self {
            dir,
            runtime: None,
            stamp: None,
            next_check: Instant::now(),
            time: 0.0,
            keys: VecDeque::new(),
            last: None,
            burst: 0,
            burst_until: None,
        };
        host.reload();
        host
    }

    /// 场景素材目录;图片与精灵的相对路径从这儿解析。
    pub(super) fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// 场景是否可用;不可用时窗口层整层跳过,一行代码都不多跑。
    pub(super) fn live(&self) -> bool {
        self.runtime.is_some()
    }

    /// 目录有变化就重新解析;坏了保留旧场景并报错,下次保存还有机会。
    pub(super) fn refresh(&mut self, now: Instant) {
        if now < self.next_check {
            return;
        }
        self.next_check = now + RELOAD_CHECK;
        let stamp = std::fs::metadata(self.dir.join("scene.toml"))
            .and_then(|meta| meta.modified())
            .ok();
        if stamp == self.stamp {
            return;
        }
        self.reload();
    }

    fn reload(&mut self) {
        let path = self.dir.join("scene.toml");
        self.stamp = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        let Ok(text) = std::fs::read_to_string(&path) else {
            if self.runtime.take().is_some() {
                println!("场景文件读不到了({}),场景层停用", path.display());
            }
            return;
        };
        match lueying_scene::parse("scene.toml", &text) {
            Ok(scene) => {
                println!("场景已加载:{}", path.display());
                self.runtime = Some(Runtime::new(scene));
            }
            Err(errors) => {
                for error in errors {
                    println!("场景解析失败:{error}");
                }
                // 只在「从有到坏」时降级;启动时就没有场景不算降级。
                if self.runtime.take().is_some() {
                    println!("场景解析失败,沿用上一个合法版本");
                }
            }
        }
    }

    /// 会话差异翻成事件;每帧组合变化后调用一次。
    pub(super) fn observe(&mut self, view: Option<&SessionView>) -> Vec<EventName> {
        let mut events = Vec::new();
        let now = Instant::now();
        match (&self.last, view) {
            (Some(old), Some(new)) => {
                if old.chinese != new.chinese {
                    events.push(EventName::ModeSwitch);
                }
                if old.page != new.page {
                    events.push(EventName::Page);
                }
                let ps = |view: &SessionView| view.page * view.page_size.max(1);
                if old.highlight.saturating_sub(ps(old)) != new.highlight.saturating_sub(ps(new)) {
                    events.push(EventName::Select);
                }
                if new.raw.len() < old.raw.len() {
                    self.burst += 1;
                    let until = self.burst_until.get_or_insert(now + BURST_WINDOW);
                    if now > *until {
                        self.burst = 1;
                        *until = now + BURST_WINDOW;
                    }
                    if self.burst >= BURST_COUNT {
                        events.push(EventName::BackspaceBurst);
                        self.burst = 0;
                        self.burst_until = None;
                    }
                } else {
                    self.burst = 0;
                    self.burst_until = None;
                }
                if new.raw != old.raw {
                    events.push(EventName::Key);
                    self.keys.push_back(now);
                } else if old.preedit != new.preedit || old.candidates != new.candidates {
                    events.push(EventName::Correction);
                }
            }
            (Some(_), None) => events.push(EventName::Commit),
            (None, Some(_)) => {
                // 新组合开始:连发计数清零,场景时钟照走。
                self.burst = 0;
                self.burst_until = None;
            }
            (None, None) => {}
        }
        self.last = view.cloned();
        events
    }

    /// 推进场景时钟并出一帧;`dt` 为 0(减少动态效果)时画面冻结。
    pub(super) fn tick(
        &mut self,
        dt: f64,
        inputs: &Inputs,
        events: &[EventName],
    ) -> Option<lueying_scene::Frame> {
        let runtime = self.runtime.as_mut()?;
        self.time += dt;
        Some(runtime.tick(self.time, dt, inputs, events))
    }

    /// 打字速率(键/秒)与空闲时长(秒);信号源的窗口层部分。
    pub(super) fn rate_and_idle(&mut self) -> (f64, f64) {
        let now = Instant::now();
        while self
            .keys
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > RATE_WINDOW)
        {
            self.keys.pop_front();
        }
        let rate = self.keys.len() as f64 / RATE_WINDOW.as_secs_f64();
        let idle = self
            .keys
            .back()
            .map_or(999.0, |at| now.saturating_duration_since(*at).as_secs_f64());
        (rate, idle)
    }
}

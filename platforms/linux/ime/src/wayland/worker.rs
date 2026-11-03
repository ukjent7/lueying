//! 输入法侧的小定时器:停顿重排与按键重复。
//!
//! Wayland 事件循环不能阻塞,推理与等待放在一个工作线程里;到期事件经
//! `calloop::channel` 回到主线程。重复速率与延迟来自键盘 grab 的
//! `repeat_info`,默认值只是 grab 事件到达前的兜底。

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// 停顿重排的静默窗口,与 Windows、macOS 一致。
const SETTLE: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug)]
pub enum Command {
    /// 重新开始停顿计时。
    Settle,
    /// 按下可重复键;`repeat` 为假时只清掉旧的重复任务。
    KeyPress { repeat: bool },
    KeyRelease,
    RepeatInfo { rate: i32, delay: i32 },
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Settle,
    Repeat,
}

pub fn spawn(events: calloop::channel::Sender<Event>) -> Sender<Command> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || run(&rx, &events));
    tx
}

fn run(rx: &mpsc::Receiver<Command>, events: &calloop::channel::Sender<Event>) {
    let mut settle: Option<Instant> = None;
    let mut repeat: Option<Instant> = None;
    let mut repeat_armed = false;
    let mut rate = 25i32;
    let mut delay = 600i32;
    loop {
        let now = Instant::now();
        if let Some(deadline) = settle
            && now >= deadline
        {
            settle = None;
            if events.send(Event::Settle).is_err() {
                return;
            }
        }
        if repeat_armed
            && let Some(deadline) = repeat
            && now >= deadline
        {
            if events.send(Event::Repeat).is_err() {
                return;
            }
            repeat = Some(Instant::now() + repeat_period(rate));
        }
        let next = [settle, repeat].into_iter().flatten().min();
        let timeout = next
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(60));
        match rx.recv_timeout(timeout) {
            Ok(Command::Settle) => settle = Some(Instant::now() + SETTLE),
            Ok(Command::KeyPress { repeat: armed }) => {
                repeat_armed = armed;
                repeat = armed.then(|| Instant::now() + Duration::from_millis(delay.max(1) as u64));
            }
            Ok(Command::KeyRelease) => {
                repeat_armed = false;
                repeat = None;
            }
            Ok(Command::RepeatInfo {
                rate: new_rate,
                delay: new_delay,
            }) => {
                rate = new_rate;
                delay = new_delay;
                if repeat_armed {
                    repeat = Some(Instant::now() + Duration::from_millis(delay.max(1) as u64));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn repeat_period(rate: i32) -> Duration {
    if rate <= 0 {
        // 速率为 0 表示禁用重复;给一个足够远的时间,下一次 KeyRelease 清掉。
        Duration::from_secs(86_400)
    } else {
        Duration::from_micros(1_000_000 / rate as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::repeat_period;
    use std::time::Duration;

    #[test]
    fn repeat_period_respects_rate_and_disabled_zero() {
        assert_eq!(repeat_period(25), Duration::from_millis(40));
        assert!(repeat_period(0) > Duration::from_secs(3600));
    }
}

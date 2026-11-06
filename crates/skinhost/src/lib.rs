//! 皮肤进程守护:输入法启动时拉起皮肤程序,皮肤崩溃只重启它,输入法不等它。
//!
//! 通信是单向的文本行:输入法把最新的状态写进皮肤进程的标准输入,皮肤读到哪一行
//! 就按哪一行画。只保留最新一条,皮肤卡住时不会堆积。皮肤程序不存在时什么也不做。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 发给皮肤进程的一条状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// 光标位置与行高(屏幕坐标,像素)。
    Caret { x: i32, y: i32, height: i32 },
    /// 候选窗已隐藏,皮肤可以收起。
    Hidden,
}

impl Frame {
    /// 编码成一行,不含换行。
    pub fn encode(self) -> String {
        match self {
            Frame::Caret { x, y, height } => format!("caret {x} {y} {height}"),
            Frame::Hidden => "hidden".to_string(),
        }
    }

    /// 解码一行;不认识或格式不对的行返回 `None`,皮肤应忽略它。
    pub fn parse(line: &str) -> Option<Frame> {
        let mut parts = line.split_whitespace();
        let frame = match parts.next()? {
            "caret" => {
                let x = parts.next()?.parse().ok()?;
                let y = parts.next()?.parse().ok()?;
                let height = parts.next()?.parse().ok()?;
                Frame::Caret { x, y, height }
            }
            "hidden" => Frame::Hidden,
            _ => return None,
        };
        parts.next().is_none().then_some(frame)
    }
}

/// 崩溃后的重启等待:第一次 1 秒,之后翻倍,最多 30 秒。
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << attempt.min(5)).min(Duration::from_secs(30))
}

/// 跑满这么久才崩溃,算作正常运行;重启等待从头算。
const STABLE: Duration = Duration::from_secs(60);
/// 等待新状态的间隔;也是检查皮肤是否已经退出的间隔。
const POLL: Duration = Duration::from_millis(200);

/// 只保留最新一条的邮箱:输入法每次只关心皮肤此刻该画什么。
#[derive(Default)]
struct Latest {
    slot: Mutex<Option<Frame>>,
    ready: Condvar,
}

impl Latest {
    fn put(&self, frame: Frame) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(frame);
        self.ready.notify_one();
    }

    fn take_timeout(&self, wait: Duration) -> Option<Frame> {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut slot, _) = self
            .ready
            .wait_timeout_while(slot, wait, |slot| slot.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        slot.take()
    }
}

/// 皮肤进程的句柄。丢掉它不会停掉守护线程;守护线程随输入法进程结束。
pub struct Skin {
    latest: Arc<Latest>,
}

impl Skin {
    /// 发送最新状态;不等待皮肤读取,也不会阻塞输入法。
    pub fn send(&self, frame: Frame) {
        self.latest.put(frame);
    }
}

/// 启动守护线程拉起 `exe`。程序不存在时直接返回,不起线程。
pub fn spawn(exe: PathBuf) -> Skin {
    let latest = Arc::new(Latest::default());
    let shared = Arc::clone(&latest);
    if exe.is_file() {
        std::thread::spawn(move || supervise(&exe, &shared));
    }
    Skin { latest }
}

fn supervise(exe: &Path, latest: &Latest) {
    let mut attempt = 0;
    loop {
        let started = Instant::now();
        run_once(exe, latest);
        if started.elapsed() >= STABLE {
            attempt = 0;
        }
        std::thread::sleep(backoff(attempt));
        attempt = attempt.saturating_add(1);
    }
}

/// 拉起一次皮肤,把新状态写进它的标准输入,直到它退出或写不进去。
fn run_once(exe: &Path, latest: &Latest) {
    let Ok(mut child) = Command::new(exe).stdin(Stdio::piped()).spawn() else {
        return;
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    loop {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            break;
        }
        let Some(frame) = latest.take_timeout(POLL) else {
            continue;
        };
        if writeln!(stdin, "{}", frame.encode())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_unknown_lines_are_ignored() {
        let caret = Frame::Caret {
            x: 120,
            y: -8,
            height: 22,
        };
        assert_eq!(Frame::parse(&caret.encode()), Some(caret));
        assert_eq!(Frame::parse(&Frame::Hidden.encode()), Some(Frame::Hidden));
        assert_eq!(Frame::parse("caret 1 2"), None);
        assert_eq!(Frame::parse("caret 1 2 3 4"), None);
        assert_eq!(Frame::parse("shake"), None);
        assert_eq!(Frame::parse(""), None);
    }

    #[test]
    fn restart_wait_doubles_then_stops_at_thirty_seconds() {
        let waits: Vec<u64> = (0..7).map(|attempt| backoff(attempt).as_secs()).collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn a_missing_program_starts_nothing() {
        let skin = spawn(PathBuf::from("/nonexistent/lueying-skin"));
        skin.send(Frame::Hidden);
    }
}

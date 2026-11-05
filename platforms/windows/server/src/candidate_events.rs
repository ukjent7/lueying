//! 候选窗状态事件:候选窗每次变化都发布最新快照,订阅的原生扩展由各自的投递线程取走。
//!
//! 窗口线程只更新一个「最新值」槽,从不等待扩展;扩展慢或挂住时中间状态被直接覆盖,
//! 不会堆积。订阅者在启动时按清单与同意记录确定,订阅后进程常驻,不因空闲收走。

use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use lueying_extmeta::consent::ConsentBook;
use lueying_extmeta::{Event, Manifest};
use lueying_extproto::candidate::{CandidateState, METHOD};
use lueying_extwasm::native::job::JobLimiter;
use lueying_extwasm::native::{NativeConfig, NativeHost};

/// 单次投递的应答预算;事件很小,超过就当扩展挂了。
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// 订阅者的内存上限;覆盖层不需要模型那么大的预算。
const MEMORY_LIMIT: u64 = 256 * 1024 * 1024;

struct Hub {
    /// 版本号与最新快照;版本号每次发布加一。
    latest: Mutex<(u64, CandidateState)>,
    changed: Condvar,
}

static HUB: LazyLock<Hub> = LazyLock::new(|| Hub {
    latest: Mutex::new((0, CandidateState::default())),
    changed: Condvar::new(),
});

/// 窗口线程调用:发布候选窗的最新快照。
pub(super) fn publish(state: CandidateState) {
    let mut latest = HUB.latest.lock().unwrap_or_else(|p| p.into_inner());
    latest.0 += 1;
    latest.1 = state;
    HUB.changed.notify_all();
}

/// 阻塞到版本号超过 `seen`,返回新的版本号与快照。
fn next_after(seen: u64) -> (u64, CandidateState) {
    let mut latest = HUB.latest.lock().unwrap_or_else(|p| p.into_inner());
    while latest.0 == seen {
        latest = HUB.changed.wait(latest).unwrap_or_else(|p| p.into_inner());
    }
    (latest.0, latest.1.clone())
}

/// 启动时调用:为每个已同意、订阅了候选窗状态的原生扩展开一条投递线程。
pub(super) fn spawn_subscribers() {
    let (Some(root), Some(consent_path)) = (
        crate::config::extensions_dir(),
        crate::config::extensions_consent_path(),
    ) else {
        return;
    };
    let book = ConsentBook::load(&consent_path);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some((id, allow_text, host)) = subscriber(&dir, &book) else {
            continue;
        };
        println!("候选窗事件:{id} 已订阅(文字权限 {allow_text})");
        let _ = std::thread::Builder::new()
            .name(format!("ext-event-{id}"))
            .spawn(move || pump(&id, allow_text, &host));
    }
}

/// 读清单、核对同意,返回订阅者的 id、文字权限与常驻宿主;不符合条件返回 `None`。
fn subscriber(dir: &Path, book: &ConsentBook) -> Option<(String, bool, NativeHost)> {
    let bytes = std::fs::read(dir.join("manifest.json")).ok()?;
    let manifest = Manifest::parse(&bytes).ok()?;
    if !manifest.is_native() || !manifest.subscribes(Event::CandidateState) {
        return None;
    }
    let check = book.check(dir, &manifest).ok()?;
    if check.required && !check.approved {
        println!("候选窗事件:{} 未同意,不订阅", manifest.id);
        return None;
    }
    let exe = dir.join(&manifest.exe);
    if !exe.is_file() {
        return None;
    }
    let config = NativeConfig {
        exe,
        args: Vec::new(),
        call_timeout: CALL_TIMEOUT,
        // 订阅后要一直在线接收事件:把空闲收走的阈值放到永远。
        idle_exit: Duration::MAX,
        readable: vec![dir.to_path_buf()],
        sandboxed: true,
    };
    let host = NativeHost::new(config)
        .with_limiter(Arc::new(JobLimiter::new(Some(MEMORY_LIMIT), None)));
    Some((manifest.id.clone(), manifest.may_read_candidate_text(), host))
}

/// 投递线程:取最新快照,按权限裁剪后发给扩展。连续失败只记一行日志。
fn pump(id: &str, allow_text: bool, host: &NativeHost) {
    let mut seen = 0;
    let mut failing = false;
    loop {
        let (version, state) = next_after(seen);
        seen = version;
        match host.call(METHOD, &state.for_subscriber(allow_text)) {
            Ok(_) => failing = false,
            Err(error) if !failing => {
                failing = true;
                println!("候选窗事件投递 {id} 失败: {error}");
            }
            Err(_) => {}
        }
    }
}

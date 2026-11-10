//! 扩展服务缝:宿主原语的注册表与阻塞调用,平台无关。
//!
//! 职责边界:这里只定义「扩展怎么调、提供者怎么回」,不含任何平台实现。
//! 提供者由宿主进程在启动时装进 [`Registry`];Wasm guest 通过宿主导入
//! 发起调用,宿主在这一层阻塞等待结果。提供者可以选择同步完成,也可以
//! 把 [`Done`] 交给截图/识别/面板线程异步完成;调用方只看见一个
//! `Result<String, String>`。接口的记录文件是 `lueying.wit`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::manifest::Service;

/// 服务缝 IDL 原文;测试按它核对方法白名单。
#[cfg(test)]
pub const WIT: &str = include_str!("../../lueying.wit");

/// 隐私字段检查:返回 `true` 时所有扩展服务调用都拒绝。
pub type PrivacyCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// 提供者回传结果的句柄;克隆便宜,可跨线程交给异步操作。
#[derive(Clone)]
pub struct Done {
    result: mpsc::Sender<Result<String, String>>,
}

impl Done {
    /// 配对一个完成句柄与它的接收端;结果要转到另一条管道(宿主请求的应答)时用。
    pub fn channel() -> (Done, mpsc::Receiver<Result<String, String>>) {
        let (sender, receiver) = mpsc::channel();
        (Done { result: sender }, receiver)
    }

    /// 完成一次调用。接收端已经超时或取消时静默丢弃。
    pub fn complete(&self, result: Result<String, String>) {
        let _ = self.result.send(result);
    }
}

/// 一个服务的提供者。方法名已由 [`Registry::call`] 按白名单核过,
/// `caller` 是发起调用的扩展 id(按扩展隔离的存储用),`args` 是
/// 服务方法的 JSON 文本;结果必须经 `done` 回传,返回值只表示「调用
/// 有没有发出去」(`Err` 表示参数坏到连结果都造不出来)。
pub trait Provider: Send + Sync {
    /// 接收一次已核过方法名的调用,结果经 `done` 回传。
    fn call(&self, caller: &str, method: &str, args: &str, done: Done) -> Result<(), String>;
}

/// 服务注册表:宿主进程装提供者,扩展宿主发调用并等待结果。
pub struct Registry {
    providers: Mutex<Vec<(Service, Arc<dyn Provider>)>>,
    /// 隐私字段检查;装有闭包时每次调用前问一次。
    privacy: Mutex<Option<PrivacyCheck>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// 空注册表;每个服务调用都会得到「没有提供者」。
    pub fn new() -> Self {
        Self {
            providers: Mutex::new(Vec::new()),
            privacy: Mutex::new(None),
        }
    }

    /// 装一个服务的提供者;同服务重复装是替换(热切换提供者用)。
    pub fn set(&self, service: Service, provider: Arc<dyn Provider>) {
        let mut providers = self.providers.lock().expect("提供者表锁");
        match providers.iter_mut().find(|(s, _)| *s == service) {
            Some(slot) => slot.1 = provider,
            None => providers.push((service, provider)),
        }
    }

    /// 装隐私检查;密码/隐私字段出现时闭包返回 `true`。
    pub fn set_privacy(&self, check: PrivacyCheck) {
        *self.privacy.lock().expect("隐私检查锁") = Some(check);
    }

    /// 是否装了某服务的提供者。
    pub fn has(&self, service: Service) -> bool {
        self.providers
            .lock()
            .expect("提供者表锁")
            .iter()
            .any(|(s, _)| *s == service)
    }

    /// 发起一次调用并等待结果。
    ///
    /// 服务名与方法名在这里核死:未知服务、没开放的接口、没装提供者
    /// 都当场拒绝,提供者只面对合法方法。`caller` 是扩展 id,供按扩展隔离的服务使用。
    pub fn call(
        &self,
        caller: &str,
        service: &str,
        method: &str,
        args: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.call_cancellable(caller, service, method, args, timeout, None)
    }

    /// 同上,另带取消标志;扩展卸载或超时时宿主置位,调用在 50 ms 内返回。
    pub fn call_cancellable(
        &self,
        caller: &str,
        service: &str,
        method: &str,
        args: &str,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<String, String> {
        let service_enum = match Service::ALL.iter().copied().find(|s| s.key() == service) {
            Some(service) => service,
            None => return Err(format!("未知服务 {service:?}")),
        };
        if !methods(service_enum).contains(&method) {
            let known = methods(service_enum).join(", ");
            let error = format!(
                "服务 {} 没有方法 {method:?}(可用: {known})",
                service_enum.key()
            );
            return Err(error);
        }
        let privacy = self
            .privacy
            .lock()
            .expect("隐私检查锁")
            .as_ref()
            .map(Arc::clone);
        if privacy.is_some_and(|check| check()) {
            return Err("隐私字段下已停用扩展服务".to_string());
        }
        let provider = self
            .providers
            .lock()
            .expect("提供者表锁")
            .iter()
            .find(|(s, _)| *s == service_enum)
            .map(|(_, p)| Arc::clone(p))
            .ok_or_else(|| format!("服务 {} 没有提供者", service_enum.key()))?;
        let (sender, receiver) = mpsc::channel();
        // 保留一个 Sender 直到调用结算:提供者返回 Ok 后可以不立即完成,
        // 若这里只传临时值,接收端会提前看到断连而误报"没有回传结果"。
        let done = Done { result: sender };
        provider.call(caller, method, args, done.clone())?;
        let deadline = Instant::now() + timeout;
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err(format!("服务 {service} 调用已取消"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("服务 {service} 调用超时({} s)", timeout.as_secs()));
            }
            match receiver.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("服务 {service} 没有回传结果"));
                }
            }
        }
    }
}

/// 各服务开放的方法;以 `lueying.wit` 为准。
///
/// 没开放的接口是空表:清单里声明了也调不到。
pub fn methods(service: Service) -> &'static [&'static str] {
    match service {
        Service::Capture => &["shot"],
        Service::Ocr => &["recognize"],
        Service::Clipboard => &["read", "write", "history", "remember", "clear"],
        Service::Store => &["get", "set", "remove", "list"],
        Service::Output => &["deliver"],
        Service::Panel => &["show"],
        Service::Hotkey => &["register", "unregister", "press"],
        Service::Notify => &["show"],
        Service::Network => &["fetch"],
        Service::Context => &["selection"],
        Service::Audio => &["record"],
        Service::Asr => &["recognize"],
    }
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;

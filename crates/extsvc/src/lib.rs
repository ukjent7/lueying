//! 扩展服务缝:宿主原语的注册表与完成队列,平台无关。
//!
//! 职责边界:这里只定义「扩展怎么调、提供者怎么回」,不含任何平台实现。
//! 提供者由宿主进程在启动时装进 [`Registry`];扩展侧 JS 只看见
//! `lueying.capture.shot()` 这类 Promise 接口。调用经 [`Done`] 完成队列
//! 异步回传,提供者选择同步还是跨线程完成都行,队列由扩展宿主在泵任务时
//! 抽干。接口的记录文件是 [`crate::WIT`](`lueying.wit`),方法名以它为准。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lueying_extmeta::Service;

/// 服务缝 IDL 原文;测试按它核对方法白名单。
pub const WIT: &str = include_str!("../lueying.wit");

/// 一次服务调用的回传:发起时的序号与结果。
///
/// 序号由扩展引导层分配,宿主拿它把结果路由回对应的 Promise;成功分支是
/// 结果 JSON 文本,失败分支是给用户看的错误文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub token: u64,
    pub result: Result<String, String>,
}

/// 提供者回传结果的句柄;克隆便宜,可跨线程交给异步操作。
#[derive(Clone)]
pub struct Done {
    token: u64,
    completions: Arc<Mutex<VecDeque<Completion>>>,
}

impl Done {
    /// 完成一次调用;接收端按 token 找等待中的 Promise,找不到就丢弃,
    /// 所以重复完成没有效果。
    pub fn complete(&self, result: Result<String, String>) {
        self.completions
            .lock()
            .expect("完成队列锁")
            .push_back(Completion {
                token: self.token,
                result,
            });
    }
}

/// 一个服务的提供者。方法名已由 [`Registry::call`] 按白名单核过,
/// `args` 是 IDL record 的 JSON 文本;结果必须经 `done` 回传,返回值只
/// 表示「调用有没有发出去」(`Err` 表示参数坏到连结果都造不出来)。
pub trait Provider: Send + Sync {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String>;
}

/// 服务注册表:宿主进程装提供者,扩展宿主发调用、抽完成。
pub struct Registry {
    providers: Mutex<Vec<(Service, Arc<dyn Provider>)>>,
    completions: Arc<Mutex<VecDeque<Completion>>>,
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
            completions: Arc::new(Mutex::new(VecDeque::new())),
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

    /// 是否装了某服务的提供者。
    pub fn has(&self, service: Service) -> bool {
        self.providers
            .lock()
            .expect("提供者表锁")
            .iter()
            .any(|(s, _)| *s == service)
    }

    /// 发起一次调用;结果经完成队列异步回传。
    ///
    /// 服务名与方法名在这里核死:未知服务、没开放的接口、没装提供者
    /// 都当场拒绝,提供者只面对合法方法。
    pub fn call(&self, service: &str, method: &str, args: &str, token: u64) -> Result<(), String> {
        let service = Service::ALL
            .iter()
            .copied()
            .find(|s| s.key() == service)
            .ok_or_else(|| format!("未知服务 {service:?}"))?;
        if !methods(service).contains(&method) {
            let known = methods(service).join(", ");
            return Err(format!(
                "服务 {} 没有方法 {method:?}(可用: {known})",
                service.key()
            ));
        }
        let provider = self
            .providers
            .lock()
            .expect("提供者表锁")
            .iter()
            .find(|(s, _)| *s == service)
            .map(|(_, p)| Arc::clone(p))
            .ok_or_else(|| format!("服务 {} 没有提供者", service.key()))?;
        let done = Done {
            token,
            completions: Arc::clone(&self.completions),
        };
        provider.call(method, args, done)
    }

    /// 是否有待抽干的完成结果;泵任务用它决定要不要空转。
    pub fn has_completions(&self) -> bool {
        !self.completions.lock().expect("完成队列锁").is_empty()
    }

    /// 抽干完成队列;扩展宿主在每轮泵任务里调用。
    pub fn take_completions(&self) -> Vec<Completion> {
        self.completions
            .lock()
            .expect("完成队列锁")
            .drain(..)
            .collect()
    }
}

/// 各服务开放的方法;以 `lueying.wit` 为准。
///
/// 没开放的接口(剪贴板、网络、存储)是空表:清单里声明了也调不到。
pub fn methods(service: Service) -> &'static [&'static str] {
    match service {
        Service::Capture => &["shot"],
        Service::Ocr => &["recognize"],
        Service::Output => &["deliver"],
        Service::Panel => &["show"],
        Service::Hotkey => &["register", "unregister"],
        Service::Notify => &["show"],
        Service::Clipboard | Service::Net | Service::Store => &[],
    }
}

#[cfg(test)]
mod registry_tests;

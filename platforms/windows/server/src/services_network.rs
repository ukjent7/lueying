//! 联网提供者:白名单/免密直通,传输转交独立 broker 进程。

use std::collections::HashMap;

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Arc;

use std::sync::Mutex;

use std::time::Duration;

use lueying_broker::{FetchArgs, FetchResult};

use lueying_extwasm::native::{NativeConfig, NativeHost};

use lueying_extwasm::{Done, Provider};

/// 联网 broker 的拉起配置:随产品分发的 `lueying-broker.exe` 与
/// server 同目录。进程不套沙箱——它是自有组件,而无能力 AppContainer
/// 没有网络权限;隔离靠进程边界与只进白名单域名的协议。
pub fn network_broker() -> Option<NativeHost> {
    let exe = std::env::current_exe().ok().and_then(|path| {
        let sibling = path.with_file_name("lueying-broker.exe");
        sibling.is_file().then_some(sibling)
    })?;
    // 时限封顶 120 s,加冷启动与排队的余量。
    let config = NativeConfig {
        exe,
        args: Vec::new(),
        call_timeout: Duration::from_secs(135),
        idle_exit: Duration::from_secs(60),
        readable: Vec::new(),
        sandboxed: false,
    };
    Some(NativeHost::new(config))
}

/// 联网提供者:白名单按扩展强制(域名清单来自扩展清单并经用户同意),
/// 请求转给独立 broker 进程;server 进程自己保持离线。
pub struct NetworkProvider {
    /// 扩展 id → 白名单;每次扩展重载时整体重建,没登记的扩展一律拒绝。
    allow: Mutex<HashMap<String, Arc<[String]>>>,
    /// 全局免密直通:跳过逐域名白名单,只保留 URL 合法性检查。
    open_all: AtomicBool,
    broker: Option<NativeHost>,
}

impl NetworkProvider {
    /// `broker` 缺席(组件没随包)时所有联网调用都报缺组件。
    pub fn new(broker: Option<NativeHost>) -> Self {
        Self {
            allow: Mutex::new(HashMap::new()),
            open_all: AtomicBool::new(false),
            broker,
        }
    }

    /// 灌入本次扩展重载的白名单;整体替换。
    pub fn set_allow(&self, grants: Vec<(String, Vec<String>)>) {
        let map = grants
            .into_iter()
            .map(|(id, allow)| (id, Arc::from(allow.into_boxed_slice())))
            .collect();
        *self.allow.lock().expect("联网白名单锁") = map;
    }

    /// 设置全局免密直通;配置变化后由扩展宿主重建时调用。
    pub fn set_open_all(&self, open: bool) {
        self.open_all.store(open, Ordering::Relaxed);
    }
}

impl Provider for NetworkProvider {
    fn call(&self, caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        let Some(broker) = self.broker.clone() else {
            done.complete(Err("联网 broker 组件缺失;重新安装输入法".into()));
            return Ok(());
        };
        let allow = self
            .allow
            .lock()
            .expect("联网白名单锁")
            .get(caller)
            .map(|allow| allow.to_vec())
            .unwrap_or_default();
        let args: FetchArgs =
            serde_json::from_str(args).map_err(|_| "联网参数不是合法 JSON".to_string())?;
        if self.open_all.load(Ordering::Relaxed) {
            if let Err(error) = lueying_broker::url_authority(&args.url) {
                done.complete(Err(error));
                return Ok(());
            }
        } else {
            match lueying_broker::host_allowed(&allow, &args.url) {
                Ok(true) => {}
                Ok(false) => {
                    let host = lueying_broker::url_authority(&args.url)
                        .map(|(host, _, _)| host)
                        .unwrap_or_else(|_| args.url.clone());
                    done.complete(Err(format!("域名 {host} 不在扩展声明的白名单里")));
                    return Ok(());
                }
                Err(error) => {
                    done.complete(Err(error));
                    return Ok(());
                }
            }
        }
        // 传输在独立线程完成;扩展卸载或停用时等待侧 50 ms 内取消,
        // 已在途的请求由 broker 进程自己跑完,结果静默丢弃。
        let request = serde_json::to_string(&args).unwrap_or_default();
        std::thread::Builder::new()
            .name("ext-network".into())
            .spawn(move || {
                done.complete(broker.call("fetch", &request).and_then(|payload| {
                    let reply: FetchResult = serde_json::from_str(&payload)
                        .map_err(|error| format!("broker 应答不合法: {error}"))?;
                    serde_json::to_string(&reply)
                        .map_err(|error| format!("broker 应答序列化失败: {error}"))
                }));
            })
            .map_err(|error| format!("联网线程启动失败: {error}"))?;
        Ok(())
    }
}

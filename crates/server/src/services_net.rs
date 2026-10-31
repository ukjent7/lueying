//! 扩展网络服务:按扩展 id 查清单白名单,交给按需拉起的代理进程。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lueying_bypass::{Bypass, BypassConfig};
use lueying_extjs::{Done, Provider};
use serde_json::json;

use super::one_method;

/// 扩展网络:按扩展 id 查清单白名单,再交给按需拉起的 `lueying-netproxy.exe`。
/// 代理进程执行 WinHTTP;常驻 Server 不链接网络代码。
pub struct NetProvider {
    domains: Arc<HashMap<String, Vec<String>>>,
    proxy: Bypass,
}

impl NetProvider {
    pub fn new(domains: HashMap<String, Vec<String>>) -> Self {
        let exe = std::env::var_os("LUEYING_NETPROXY")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join("lueying-netproxy.exe")))
            })
            .unwrap_or_else(|| PathBuf::from("lueying-netproxy.exe"));
        Self {
            domains: Arc::new(domains),
            proxy: Bypass::new(BypassConfig {
                exe,
                args: Vec::new(),
                call_timeout: Duration::from_secs(25),
            }),
        }
    }
}

impl Provider for NetProvider {
    fn call(&self, caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("net", method, "fetch")?;
        let domains = self.domains.get(caller).cloned().unwrap_or_default();
        if domains.is_empty() {
            done.complete(Err(format!(
                "扩展 {caller} 没有声明 network.domains,网络请求被拒绝"
            )));
            return Ok(());
        }
        let mut parsed: serde_json::Value =
            serde_json::from_str(args).map_err(|error| error.to_string())?;
        parsed["domains"] = json!(domains);
        let proxy = self.proxy.clone();
        let args = parsed.to_string();
        std::thread::Builder::new()
            .name("ext-net".to_string())
            .spawn(move || done.complete(proxy.call("fetch", &args)))
            .map_err(|error| format!("网络代理线程启动失败: {error}"))?;
        Ok(())
    }
}

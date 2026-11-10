//! 设置窗的出站 HTTP:更新检查、安装包下载与扩展安装共用。外部地址只许 HTTPS,
//! 证书走系统证书库(企业根证书也认),HTTPS 不降级到 HTTP。代理按 `lueying_ext::proxy` 选。

use std::time::Duration;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

/// 访问 `url` 用的 HTTPS 客户端,带整体时限。非 2xx 按错误返回,调用方据 `StatusCode` 判断。
pub fn agent(url: &str, timeout: Duration) -> Result<Agent, String> {
    let proxy = match lueying_ext::proxy::proxy_for(url) {
        Some(proxy) => Some(
            ureq::Proxy::new(&proxy).map_err(|error| format!("代理设置不可用 {proxy}: {error}"))?,
        ),
        None => None,
    };
    Ok(Agent::config_builder()
        .timeout_global(Some(timeout))
        .https_only(true)
        .proxy(proxy)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into())
}

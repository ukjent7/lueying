//! 设置窗的出站 HTTP:更新检查、安装包下载与扩展安装共用。外部地址只许 HTTPS,
//! 证书走系统证书库(企业根证书也认),HTTPS 不降级到 HTTP。

use std::time::Duration;

use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

/// 一个带整体时限的 HTTPS 客户端;非 2xx 按错误返回,调用方据 `StatusCode` 判断。
pub fn agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .timeout_global(Some(timeout))
        .https_only(true)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

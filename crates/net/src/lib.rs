//! 联网边界:常驻输入服务不依赖本 crate。
//!
//! 更新检查、GitHub Release 读取与 WinHTTP 下载都只出现在这里。
//! `lueying-server` 二进制不引用本 crate,CI 再用导入表断言兜底;
//! 设置窗与 `lueying-update.exe` 通过它联网。

pub mod update_manifest;

#[cfg(windows)]
pub mod http;
#[cfg(windows)]
pub mod proxy;
#[cfg(windows)]
pub mod update;

//! 进程级设置:本机数据目录与 OCR 后端。启动参数与配置消息定下,之后只读。

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// 宿主用到的本机目录,由服务端启动时传入。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    /// 数据目录(`%APPDATA%\lueying`):扩展包、同意记录、剪贴板历史。
    pub data: PathBuf,
    /// 本地数据目录(`%LOCALAPPDATA%\lueying`):运行指标与日志。
    pub local: PathBuf,
}

impl Paths {
    /// 已安装扩展目录。
    pub fn extensions(&self) -> PathBuf {
        self.data.join("extensions")
    }

    /// 扩展同意记录。
    pub fn consent(&self) -> PathBuf {
        self.extensions().join("consent.json")
    }

    /// 扩展运行指标快照。
    pub fn metrics(&self) -> PathBuf {
        self.local.join("logs").join("ext-metrics.json")
    }

    /// 扩展持久存储目录,每个扩展一个 JSON 文件;`LUEYING_EXT_DATA_DIR` 可覆盖(测试用)。
    pub fn ext_data(&self) -> PathBuf {
        match std::env::var_os("LUEYING_EXT_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => self.data.join("ext-data"),
        }
    }

    /// 剪贴板历史文件。
    pub fn clipboard_history(&self) -> PathBuf {
        self.data.join("clipboard-history.json")
    }
}

static PATHS: OnceLock<Paths> = OnceLock::new();
static PPOCR: AtomicBool = AtomicBool::new(false);

/// 装上本机目录;只生效一次。
pub fn install(paths: Paths) {
    let _ = PATHS.set(paths);
}

/// 本机目录;没装过时是空路径(单测用)。
pub fn paths() -> &'static Paths {
    PATHS.get_or_init(Paths::default)
}

/// 更新 OCR 后端选择(配置消息到达时)。
pub fn set_ppocr(on: bool) {
    PPOCR.store(on, Ordering::Relaxed);
}

/// OCR 后端是否选了 PP-OCR。
#[cfg(windows)]
pub fn ppocr() -> bool {
    PPOCR.load(Ordering::Relaxed)
}

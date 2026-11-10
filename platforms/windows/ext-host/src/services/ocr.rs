//! 文字识别:系统 OCR 或 PP-OCRv5 原生扩展,外加原生扩展宿主 glue。

use std::collections::HashMap;

use std::path::Path;

use std::sync::Arc;

use std::sync::Mutex;

use std::time::Duration;

use lueying_ext::manifest::consent::ConsentBook;

use lueying_ext::host::native::{NativeConfig, NativeHost};

use lueying_ext::host::{Done, Provider};

use serde_json::json;

use windows::Globalization::Language;

use windows::Graphics::Imaging::BitmapDecoder;

use windows::Media::Ocr::OcrEngine;

use windows::Storage::{FileAccessMode, StorageFile};

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use windows::core::HSTRING;

use super::one_method;

use super::clipboard::record_tagged_clipboard;

/// 文字识别;默认系统 OCR,配置 `ocr_engine=ppocr` 或调用参数 `engine` 时
/// 走 PP-OCRv6 原生扩展(独立进程,模型常驻)。`lang` 只对系统 OCR 有意义。
pub struct OcrProvider {
    native: Arc<NativeHosts>,
}

impl OcrProvider {
    pub fn new() -> Self {
        Self {
            native: Arc::new(NativeHosts::default()),
        }
    }
}

impl Provider for OcrProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("ocr", method, "recognize")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let file = parsed["file"]
            .as_str()
            .ok_or_else(|| "缺 file".to_string())?
            .to_string();
        let lang = parsed["lang"].as_str().map(str::to_string);
        let engine = parsed["engine"].as_str().map(str::to_string);
        if std::env::var_os("LUEYING_FAKE_OCR").is_some() {
            done.complete(fake_ocr(&file));
            return Ok(());
        }
        let native = Arc::clone(&self.native);
        std::thread::Builder::new()
            .name("ext-ocr".into())
            .spawn(move || {
                let result = recognize_file(&native, &file, lang, engine);
                if let Ok(payload) = &result {
                    remember_ocr_text(payload);
                }
                done.complete(result);
            })
            .map_err(|e| format!("识别线程启动失败: {e}"))?;
        Ok(())
    }
}

/// OCR 结果静默收进剪贴板历史,带 `OCR` 标签;正文为空或格式不符时跳过。
fn remember_ocr_text(payload: &str) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload)
        && let Some(text) = value["text"].as_str()
    {
        record_tagged_clipboard(text.trim(), "OCR");
    }
}

/// 原生扩展宿主侧:按 id 找包、查同意、按需构建常驻进程。进程由
/// [`lueying_ext::host::native::NativeHost`] 管;同意指纹含可执行文件哈希,在
/// 拉起进程前核对,文件一变同意失效、宿主随之重建。进程存活期间
/// Windows 文件锁保证可执行文件就是核对过的那份。
#[derive(Default)]
pub struct NativeHosts {
    hosts: Mutex<HashMap<String, Arc<NativeHost>>>,
}

impl NativeHosts {
    /// 调一次原生扩展;包缺失、清单不对或未同意都是明确错误。
    pub fn call(&self, id: &str, method: &str, args: &str) -> Result<String, String> {
        let host = self.boot(id)?;
        host.call(method, args)
    }

    /// 冷启动:核对清单与同意指纹后拉起常驻进程。只在进程未存活时
    /// 执行,识别热路径不重算整包哈希。
    fn boot(&self, id: &str) -> Result<Arc<NativeHost>, String> {
        {
            let hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(host) = hosts.get(id) {
                return Ok(Arc::clone(host));
            }
        }
        let dir = crate::state::paths().extensions().join(id);
        let bytes = std::fs::read(dir.join("manifest.json"))
            .map_err(|error| format!("读 {id} 清单失败: {error}"))?;
        let manifest = lueying_ext::manifest::Manifest::parse(&bytes)?;
        if !manifest.is_native() {
            return Err(format!("{id} 不是原生扩展"));
        }
        let book = ConsentBook::load(&crate::state::paths().consent());
        let check = book.check(&dir, &manifest)?;
        if check.required && !check.approved {
            return Err(format!("{id} 需在设置里同意后才能调用"));
        }
        let exe = dir.join(&manifest.exe);
        if !exe.is_file() {
            return Err(format!("原生扩展 {id} 缺可执行文件 {}", manifest.exe));
        }
        let mut hosts = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(host) = hosts.get(id) {
            return Ok(Arc::clone(host));
        }
        let host = Arc::new(native_host(&exe));
        hosts.insert(id.to_string(), Arc::clone(&host));
        Ok(host)
    }
}

/// 拉起配置:识别给 120 s,空闲 60 s 收进程。
fn native_host(exe: &Path) -> NativeHost {
    NativeHost::new(NativeConfig {
        exe: exe.to_path_buf(),
        args: Vec::new(),
        call_timeout: Duration::from_secs(120),
        idle_exit: Duration::from_secs(60),
    })
}

/// 按调用参数或配置选引擎;参数优先,未知值回配置。
fn recognize_file(
    native: &NativeHosts,
    file: &str,
    lang: Option<String>,
    engine: Option<String>,
) -> Result<String, String> {
    let ppocr = match engine.as_deref() {
        Some("ppocr" | "pp-ocr" | "paddle") => true,
        Some("system" | "windows") => false,
        _ => crate::state::ppocr(),
    };
    if ppocr {
        native.call("ppocr", "recognize", &json!({ "file": file }).to_string())
    } else {
        ocr_file(file, lang)
    }
}

/// Windows E2E 的固定识别结果:CI 镜像不一定带 OCR 语言包,截图与上屏仍是
/// 真实现,只有识别这一步换成固定文本;顺带校验截图文件确实是 BMP。
const FAKE_OCR_TEXT: &str = "掠影截图识别测试";

fn fake_ocr(file: &str) -> Result<String, String> {
    match std::fs::read(file) {
        Ok(bytes) if bytes.starts_with(b"BM") => {
            Ok(json!({ "text": FAKE_OCR_TEXT, "lang": "fake" }).to_string())
        }
        Ok(_) => Err("假识别:截图不是 BMP".into()),
        Err(error) => Err(format!("假识别读截图失败: {error}")),
    }
}

/// 识别一张位图文件;线程自带 MTA 初始化,与宿主 STA 隔离。
fn ocr_file(file: &str, lang: Option<String>) -> Result<String, String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;
    }
    let outcome = ocr_inner(file, lang);
    unsafe { CoUninitialize() };
    outcome
}

fn ocr_inner(file: &str, lang: Option<String>) -> Result<String, String> {
    let path = HSTRING::from(file);
    let storage = StorageFile::GetFileFromPathAsync(&path)
        .and_then(|operation| operation.join())
        .map_err(|e| format!("GetFileFromPathAsync: {e}"))?;
    let stream = storage
        .OpenAsync(FileAccessMode::Read)
        .and_then(|operation| operation.join())
        .map_err(|e| format!("OpenAsync: {e}"))?;
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|operation| operation.join())
        .map_err(|e| format!("BitmapDecoder: {e}"))?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .and_then(|operation| operation.join())
        .map_err(|e| format!("GetSoftwareBitmapAsync: {e}"))?;
    let engine = match lang {
        Some(tag) => requested_engine(&tag)?,
        None => fallback_engine()?,
    };
    let recognized = engine
        .RecognizeAsync(&bitmap)
        .and_then(|operation| operation.join())
        .map_err(|e| format!("RecognizeAsync: {e}"))?;
    let text = recognized
        .Text()
        .map_err(|e| format!("OcrResult.Text: {e}"))?
        .to_string();
    let actual = engine
        .RecognizerLanguage()
        .and_then(|language| language.LanguageTag())
        .map(|tag| tag.to_string())
        .unwrap_or_default();
    serde_json::to_string(&json!({ "text": text, "lang": actual })).map_err(|e| e.to_string())
}

/// 请求的语言受支持就用它;不支持时记下原因走回退,结果里的 lang 说明实情。
fn requested_engine(tag: &str) -> Result<OcrEngine, String> {
    let language = Language::CreateLanguage(&HSTRING::from(tag))
        .map_err(|e| format!("建语言对象失败: {e}"))?;
    if OcrEngine::IsLanguageSupported(&language).map_err(|e| format!("IsLanguageSupported: {e}"))? {
        OcrEngine::TryCreateFromLanguage(&language)
            .map_err(|e| format!("请求语言 {tag} 创建引擎失败: {e}"))
    } else {
        fallback_engine()
    }
}

fn fallback_engine() -> Result<OcrEngine, String> {
    OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "没有可用 OCR 引擎(语言包缺失)".to_string())
}

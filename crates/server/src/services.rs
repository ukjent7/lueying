//! Windows 服务提供者:T3 服务缝的平台实现。
//!
//! 提供者从扩展宿主线程被 [`lueying_extjs::Registry::call`] 调进来,慢操作
//! (截图、识别)在专用线程跑完经完成队列回传,不占派发预算。`output` 用
//! Unicode 注入把文本送进当前焦点,`panel` 的宿主面板还在后续阶段,当前回
//! 错误文本,扩展拿到明确原因。
//!
//! 截图文件落在临时目录 `lueying-capture`,文件名带毫秒时间戳;旧文件的
//! 清理随 T8 的预算与可见性一起做。热键在扩展线程上注册,WM_HOTKEY 由
//! [`HotkeyProvider::pump`] 在同一线程收取:扩展 `press` 就地兑现,用户
//! 绑定行号回带给调用方派发。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lueying_extjs::bindings::Modifier;
use lueying_extjs::{Done, Provider};
use serde_json::json;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPFILEHEADER, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOD_ALT,
    MOD_CONTROL, MOD_SHIFT, MOD_WIN, RegisterHotKey, SendInput, UnregisterHotKey, VK_BACK,
    VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP, VkKeyScanW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MSG, PM_REMOVE, PeekMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WM_HOTKEY,
};
use windows::core::HSTRING;

/// 单方法服务的入口守卫:方法名不符时回 provider 侧错误,与 [`Provider`] 约定一致。
fn one_method(provider: &str, method: &str, expected: &str) -> Result<(), String> {
    if method == expected {
        Ok(())
    } else {
        Err(format!("{provider} 没有方法 {method:?}"))
    }
}

/// 截屏;`region` 缺省是整个虚拟桌面。在调用线程上执行 GDI 全程。
pub struct CaptureProvider;

impl Provider for CaptureProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("capture", method, "shot")?;
        let region = parse_region(args)?;
        std::thread::Builder::new()
            .name("ext-capture".into())
            .spawn(move || {
                let result = shot_bmp(region).and_then(|(file, width, height)| {
                    serde_json::to_string(&json!({
                        "file": file.to_string_lossy(),
                        "width": width,
                        "height": height,
                    }))
                    .map_err(|e| e.to_string())
                });
                done.complete(result);
            })
            .map_err(|e| format!("截图线程启动失败: {e}"))?;
        Ok(())
    }
}

/// 截图区域;虚拟桌面坐标。
struct Region {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// `{"region":{"x":..,"y":..,"width":..,"height":..}}` 或缺省整屏。
fn parse_region(args: &str) -> Result<Option<Region>, String> {
    if args.is_empty() {
        return Ok(None);
    }
    let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let Some(region) = parsed.get("region") else {
        return Ok(None);
    };
    if region.is_null() {
        return Ok(None);
    }
    let field = |name: &str| -> Result<i32, String> {
        region[name]
            .as_i64()
            .map(|value| value.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
            .ok_or_else(|| format!("region.{name} 缺失或不是整数"))
    };
    Ok(Some(Region {
        x: field("x")?,
        y: field("y")?,
        width: field("width")?,
        height: field("height")?,
    }))
}

/// GDI 截一块屏幕写 32 位 BMP;返回 (文件, 宽, 高)。
fn shot_bmp(region: Option<Region>) -> Result<(PathBuf, i32, i32), String> {
    let (x, y, width, height) = match region {
        Some(region) => (region.x, region.y, region.width, region.height),
        None => unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        },
    };
    if width <= 0 || height <= 0 {
        return Err(format!("截图区域为空({width}x{height})"));
    }
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = height;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let section = CreateDIBSection(Some(memory), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        let previous = SelectObject(memory, section.into());
        let _ = BitBlt(memory, 0, 0, width, height, Some(screen), x, y, SRCCOPY);
        let stride = width as usize * 4;
        let pixels =
            std::slice::from_raw_parts(bits as *const u8, stride * height as usize).to_vec();
        SelectObject(memory, previous);
        DeleteObject(section.into());
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(None, screen);

        let mut file_header = BITMAPFILEHEADER::default();
        file_header.bfType = 0x4d42;
        file_header.bfSize = (54 + pixels.len()) as u32;
        file_header.bfOffBits = 54;
        let header_bytes = std::slice::from_raw_parts(
            (&file_header as *const BITMAPFILEHEADER) as *const u8,
            std::mem::size_of::<BITMAPFILEHEADER>(),
        );
        let info_bytes = std::slice::from_raw_parts(
            (&BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            } as *const BITMAPINFOHEADER) as *const u8,
            std::mem::size_of::<BITMAPINFOHEADER>(),
        );
        let dir = std::env::temp_dir().join("lueying-capture");
        std::fs::create_dir_all(&dir).map_err(|e| format!("建截图目录失败: {e}"))?;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("shot-{millis}-{}.bmp", std::process::id()));
        let mut bytes = Vec::with_capacity(pixels.len() + 54);
        bytes.extend_from_slice(header_bytes);
        bytes.extend_from_slice(info_bytes);
        bytes.extend_from_slice(&pixels);
        std::fs::write(&path, bytes).map_err(|e| format!("写截图失败: {e}"))?;
        Ok((path, width, height))
    }
}

/// 文字识别;默认系统 OCR,配置 `ocr_engine=ppocr` 或调用参数 `engine` 时
/// 走随包的 PP-OCRv6。`lang` 只对系统 OCR 有意义,PP-OCR 结果里报 `ppocr`。
pub struct OcrProvider {
    cache: Arc<PpOcrCache>,
}

#[derive(Default)]
struct PpOcrCache {
    /// 模型目录与已加载计划;目录换了就重新加载。
    model: Mutex<Option<(PathBuf, Arc<lueying_ocr::PpOcr>)>>,
}

impl OcrProvider {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(PpOcrCache::default()),
        }
    }
}

impl PpOcrCache {
    fn load(&self, dir: PathBuf) -> Result<Arc<lueying_ocr::PpOcr>, String> {
        let mut slot = self.model.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((cached, model)) = slot.as_ref()
            && cached == &dir
        {
            return Ok(Arc::clone(model));
        }
        let started = std::time::Instant::now();
        let model = Arc::new(lueying_ocr::PpOcr::open(&dir)?);
        println!(
            "PP-OCRv6 模型 ← {}（{} ms）",
            dir.display(),
            started.elapsed().as_millis()
        );
        *slot = Some((dir, Arc::clone(&model)));
        Ok(model)
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
        let cache = Arc::clone(&self.cache);
        std::thread::Builder::new()
            .name("ext-ocr".into())
            .spawn(move || {
                done.complete(recognize_file(&cache, &file, lang, engine));
            })
            .map_err(|e| format!("识别线程启动失败: {e}"))?;
        Ok(())
    }
}

/// 按调用参数或配置选引擎;参数优先,未知值回配置。
fn recognize_file(
    cache: &PpOcrCache,
    file: &str,
    lang: Option<String>,
    engine: Option<String>,
) -> Result<String, String> {
    let backend = match engine.as_deref() {
        Some("ppocr" | "pp-ocr" | "paddle") => crate::config::OcrBackend::Ppocr,
        Some("system" | "windows") => crate::config::OcrBackend::System,
        _ => crate::config::path()
            .map(|path| crate::config::load(&path).ocr_backend)
            .unwrap_or_default(),
    };
    match backend {
        crate::config::OcrBackend::Ppocr => ppocr_file(cache, file),
        crate::config::OcrBackend::System => ocr_file(file, lang),
    }
}

/// PP-OCRv6 只吃 BMP;截图链路固定产出 BMP,其它格式回明确错误。
fn ppocr_file(cache: &PpOcrCache, file: &str) -> Result<String, String> {
    let dir = crate::config::ppocr_dir()
        .ok_or_else(|| "PP-OCRv6 模型未安装:需要 det.onnx、rec.onnx、dict.txt".to_string())?;
    let model = cache.load(dir)?;
    let image = lueying_ocr::load_bmp(Path::new(file))?;
    let lines = model.recognize(&image)?;
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::to_string(&json!({ "text": text, "lang": "ppocr" })).map_err(|e| e.to_string())
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

/// 系统通知;v1 只落日志,toast 需要包标识或快捷方式激活,挂后续可见性任务。
pub struct NotifyProvider;

impl Provider for NotifyProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("notify", method, "show")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let title = parsed["title"].as_str().unwrap_or_default();
        let body = parsed["body"].as_str().unwrap_or_default();
        println!("扩展通知:{title}:{body}");
        done.complete(Ok("{}".into()));
        Ok(())
    }
}

/// 宿主面板(声明式列表挑选)在后续阶段;当前回错误文本,扩展拿到明确原因。
pub struct PanelProvider;

impl Provider for PanelProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        let _ = args;
        one_method("panel", method, "show")?;
        done.complete(Err("宿主面板在后续阶段".into()));
        Ok(())
    }
}

/// 上屏:把文本按 Unicode 注入当前焦点。`delivered:false` 只表示没有可送的字。
pub struct OutputProvider;

impl Provider for OutputProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("output", method, "deliver")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let text = parsed["text"]
            .as_str()
            .ok_or_else(|| "缺 text".to_string())?;
        match deliver_text(text) {
            Ok(delivered) => {
                done.complete(Ok(json!({ "delivered": delivered }).to_string()));
                Ok(())
            }
            Err(error) => {
                done.complete(Err(error));
                Ok(())
            }
        }
    }
}

/// 扩展私有存储:每个扩展一个 JSON 文件,调用方由 `Provider::call` 的
/// `caller` 决定,不能读写其它扩展的数据。键与扩展 id 同一套字符规则,
/// 单值上限 64 KiB,整份文件上限 256 KiB。
pub struct StoreProvider;

/// 单个值的上限;扩展存的是设置与缓存,不该把用户磁盘当数据库。
const STORE_VALUE_LIMIT: usize = 64 * 1024;
/// 每个扩展整份存储的上限。
const STORE_FILE_LIMIT: usize = 256 * 1024;

impl Provider for StoreProvider {
    fn call(&self, caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        if !lueying_extmeta::valid_id(caller) {
            done.complete(Err("扩展 id 不合法".to_string()));
            return Ok(());
        }
        let result = match method {
            "get" => store_get(caller, args),
            "set" => store_set(caller, args),
            "remove" => store_remove(caller, args),
            "list" => store_list(caller),
            _ => Err(format!("store 没有方法 {method:?}")),
        };
        done.complete(result);
        Ok(())
    }
}

fn store_get(ext: &str, args: &str) -> Result<String, String> {
    let key = store_key(args)?;
    let values = store_load(ext);
    let value = values.get(&key).and_then(serde_json::Value::as_str);
    Ok(json!({ "value": value }).to_string())
}

fn store_set(ext: &str, args: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let key = parsed["key"].as_str().ok_or_else(|| "缺 key".to_string())?;
    if !lueying_extmeta::valid_id(key) {
        return Err("存储键不合法".to_string());
    }
    let value = parsed["value"]
        .as_str()
        .ok_or_else(|| "缺 value".to_string())?;
    if value.len() > STORE_VALUE_LIMIT {
        return Err(format!("存储值超过 {STORE_VALUE_LIMIT} 字节"));
    }
    let mut values = store_load(ext);
    values.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    store_save(ext, &values)?;
    Ok("{}".to_string())
}

fn store_remove(ext: &str, args: &str) -> Result<String, String> {
    let key = store_key(args)?;
    let mut values = store_load(ext);
    values.remove(&key);
    store_save(ext, &values)?;
    Ok("{}".to_string())
}

fn store_list(ext: &str) -> Result<String, String> {
    let keys: Vec<String> = store_load(ext).into_iter().map(|(key, _)| key).collect();
    Ok(json!({ "keys": keys }).to_string())
}

fn store_key(args: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let key = parsed["key"].as_str().ok_or_else(|| "缺 key".to_string())?;
    if !lueying_extmeta::valid_id(key) {
        return Err("存储键不合法".to_string());
    }
    Ok(key.to_string())
}

fn store_load(ext: &str) -> serde_json::Map<String, serde_json::Value> {
    let Some(path) = store_path(ext) else {
        return serde_json::Map::new();
    };
    std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn store_save(
    ext: &str,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let path = store_path(ext).ok_or("没有可写的扩展存储目录")?;
    let Some(dir) = path.parent() else {
        return Err("扩展存储路径没有父目录".to_string());
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("创建扩展存储目录: {e}"))?;
    let text = serde_json::to_vec(values).map_err(|e| format!("序列化扩展存储: {e}"))?;
    if text.len() > STORE_FILE_LIMIT {
        return Err(format!("扩展存储超过 {STORE_FILE_LIMIT} 字节"));
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("创建扩展存储: {e}"))?;
    std::io::Write::write_all(&mut file, &text).map_err(|e| format!("写扩展存储: {e}"))?;
    file.sync_all().map_err(|e| format!("同步扩展存储: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换扩展存储: {e}"))
}

fn store_path(ext: &str) -> Option<PathBuf> {
    Some(config::ext_data_dir()?.join(format!("{ext}.json")))
}

/// 剪贴板:纯文本读写。Windows 的剪贴板是进程共享状态,打开期间其它
/// 程序会被挡住;这里只在调用线程上短暂持有,不把锁带进异步线程。
pub struct ClipboardProvider;

/// `CF_UNICODETEXT` 的标准值;避免为单个常量引入整个剪贴板格式枚举。
const CF_UNICODETEXT: u32 = 13;

impl Provider for ClipboardProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        match method {
            "read" => {
                done.complete(read_clipboard().map(|text| json!({ "text": text }).to_string()));
            }
            "write" => {
                let parsed: serde_json::Value =
                    serde_json::from_str(args).map_err(|e| e.to_string())?;
                let text = parsed["text"]
                    .as_str()
                    .ok_or_else(|| "缺 text".to_string())?;
                done.complete(write_clipboard(text).map(|()| "{}".to_string()));
            }
            _ => return Err(format!("clipboard 没有方法 {method:?}")),
        }
        Ok(())
    }
}

fn read_clipboard() -> Result<String, String> {
    unsafe {
        OpenClipboard(None).map_err(|e| format!("打开剪贴板失败: {e}"))?;
        let result = (|| {
            let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else {
                return Ok(String::new());
            };
            let memory = HGLOBAL(handle.0);
            let pointer = GlobalLock(memory).map_err(|e| format!("锁定剪贴板内存失败: {e}"))?;
            let data = pointer as *const u16;
            let mut length = 0usize;
            while *data.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(data, length));
            let _ = GlobalUnlock(memory);
            Ok(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

fn write_clipboard(text: &str) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        OpenClipboard(None).map_err(|e| format!("打开剪贴板失败: {e}"))?;
        let result = (|| {
            EmptyClipboard().map_err(|e| format!("清空剪贴板失败: {e}"))?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2)
                .map_err(|e| format!("分配剪贴板内存失败: {e}"))?;
            let pointer = GlobalLock(memory).map_err(|e| format!("锁定剪贴板内存失败: {e}"))?;
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer as *mut u16, units.len());
            let _ = GlobalUnlock(memory);
            match SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) {
                Ok(_) => Ok(()),
                Err(e) => {
                    let _ = GlobalFree(memory);
                    Err(format!("写入剪贴板失败: {e}"))
                }
            }
        })();
        let _ = CloseClipboard();
        result
    }
}

/// 把文本按 Unicode 注入当前焦点。走 `SendInput` 的 VK_PACKET 通道,不经过
/// 键盘布局与输入法组句;Electron 与自绘控件收 WM_CHAR,不依赖 TSF 会话。
/// 换行按回车送(编辑控件里就是分段),制表符换成空格,避免焦点在对话框里跳走。
/// 对外 commit 通道(`Request::ExternalCommit`)复用同一条上屏路径。
pub fn deliver_text(text: &str) -> Result<bool, String> {
    let units = deliver_units(text);
    if units.is_empty() {
        return Ok(false);
    }
    // 一次送太多事件会拖住调用线程;256 字一批,失败立即停。
    for chunk in units.chunks(256) {
        let mut inputs = Vec::with_capacity(chunk.len() * 2);
        for &unit in chunk {
            inputs.push(unicode_input(unit, false));
            inputs.push(unicode_input(unit, true));
        }
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) } as usize;
        if sent != inputs.len() {
            let error = std::io::Error::last_os_error();
            return Err(format!("上屏被系统拦截({sent}/{}):{error}", inputs.len()));
        }
    }
    Ok(true)
}

/// 文本 → 要注入的 UTF-16 单元:换行按回车、制表符换空格,孤立 CR 丢弃
/// (CRLF 先归一,避免一个换行变成两个空行);代理对保持两个单元。
fn deliver_units(text: &str) -> Vec<u16> {
    let normalized = text.replace("\r\n", "\n");
    let mut units = Vec::with_capacity(normalized.len());
    for ch in normalized.chars() {
        match ch {
            '\n' => units.push('\r' as u16),
            '\t' => units.push(' ' as u16),
            '\r' => {}
            _ => {
                let mut buf = [0u16; 2];
                units.extend_from_slice(ch.encode_utf16(&mut buf));
            }
        }
    }
    units
}

/// 一个 Unicode 键事件;`up` 决定按下还是抬起。
fn unicode_input(unit: u16, up: bool) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki.wScan = unit;
    input.Anonymous.ki.dwFlags = if up {
        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
    } else {
        KEYEVENTF_UNICODE
    };
    input
}

/// 全局热键:注册/冲突/注销语义不变;`press` 让扩展等一次触发,用户绑定
/// 经 [`HotkeyProvider::bind`] 注册、[`HotkeyProvider::pump`] 回带触发行。
/// 所有注册与收取都发生在扩展线程上,RegisterHotKey 的线程亲和性成立。
#[derive(Default)]
pub struct HotkeyProvider {
    state: Mutex<HotkeyState>,
}

#[derive(Default)]
struct HotkeyState {
    /// 扩展热键 id → RegisterHotKey 的整数 id。
    map: HashMap<String, i32>,
    /// 用户绑定行号 → 整数 id。
    user: HashMap<usize, i32>,
    /// 命令面板总热键的整数 id。
    panel: Option<i32>,
    /// 面板热键触发过,等 `take_panel` 取走。
    panel_fired: bool,
    /// 扩展挂起的 press;每个热键事件兑现一个。
    presses: HashMap<String, VecDeque<Done>>,
    next: i32,
}

impl HotkeyState {
    fn take_id(&mut self) -> i32 {
        let id = self.next;
        self.next = if self.next >= 0xBFFF {
            1
        } else {
            self.next + 1
        };
        id
    }
}

impl Provider for HotkeyProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        match method {
            "register" => self.register(args, done),
            "unregister" => self.unregister(args, done),
            "press" => self.press(args, done),
            _ => Err(format!("hotkey 没有方法 {method:?}")),
        }
    }
}

impl HotkeyProvider {
    /// 注册一条用户绑定;行号在 [`HotkeyProvider::pump`] 的返回里原样带回。
    /// 组合串按绑定表语法校验(两个修饰键、保留组合),冲突报 OS 错误。
    pub fn bind(&self, row: usize, combo: &str) -> Result<(), String> {
        let (modifiers, key) = parse_combo(combo)?;
        let mut state = self.state.lock().unwrap();
        if let Some(&old) = state.user.get(&row) {
            let _ = unsafe { UnregisterHotKey(None, old) };
        }
        let hot_id = state.take_id();
        match unsafe { RegisterHotKey(None, hot_id, modifiers, key) } {
            Ok(()) => {
                state.user.insert(row, hot_id);
                Ok(())
            }
            Err(error) => Err(format!("热键注册失败: {error}")),
        }
    }

    /// 注销全部用户绑定;绑定表重建前调用,行号从零重排。
    pub fn unbind_users(&self) {
        let mut state = self.state.lock().unwrap();
        let ids = state.user.values().copied().collect::<Vec<_>>();
        for hot_id in ids {
            let _ = unsafe { UnregisterHotKey(None, hot_id) };
        }
        state.user.clear();
    }

    /// 注册命令面板的总热键;组合串走同一套校验(两个修饰键、保留组合)。
    pub fn bind_panel(&self, combo: &str) -> Result<(), String> {
        let (modifiers, key) = parse_combo(combo)?;
        let mut state = self.state.lock().unwrap();
        if let Some(old) = state.panel.take() {
            let _ = unsafe { UnregisterHotKey(None, old) };
        }
        let hot_id = state.take_id();
        match unsafe { RegisterHotKey(None, hot_id, modifiers, key) } {
            Ok(()) => {
                state.panel = Some(hot_id);
                Ok(())
            }
            Err(error) => Err(format!("热键注册失败: {error}")),
        }
    }

    /// 注销命令面板总热键;配置清空或改组合时先摘旧注册。
    pub fn unbind_panel(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(old) = state.panel.take() {
            let _ = unsafe { UnregisterHotKey(None, old) };
        }
        state.panel_fired = false;
    }

    /// 取走一次面板热键触发;没有触发返回 `false`。
    pub fn take_panel(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        std::mem::take(&mut state.panel_fired)
    }

    /// 注销全部热键并丢弃挂起的 press;运行期换扩展时整表重建。
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        let ids = state
            .map
            .values()
            .copied()
            .chain(state.user.values().copied())
            .chain(state.panel)
            .collect::<Vec<_>>();
        for hot_id in ids {
            let _ = unsafe { UnregisterHotKey(None, hot_id) };
        }
        state.map.clear();
        state.user.clear();
        state.panel = None;
        state.panel_fired = false;
        state.presses.clear();
    }

    /// 隐私字段出现时丢弃挂起的 press;扩展拿到明确拒绝,不留悬空 Promise。
    pub fn cancel_presses(&self, reason: &str) {
        let mut state = self.state.lock().unwrap();
        for (_, queued) in state.presses.drain() {
            for done in queued {
                done.complete(Err(reason.to_string()));
            }
        }
    }

    /// 收一次消息队列:扩展挂起的 press 就地兑现,用户绑定行号回带给
    /// 调用方派发。扩展线程每轮循环调一次;没有热键时只有一次 PeekMessage。
    pub fn pump(&self) -> Vec<usize> {
        let mut fired = Vec::new();
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message != WM_HOTKEY {
                    continue;
                }
                let hot_id = msg.wParam.0 as i32;
                let mut state = self.state.lock().unwrap();
                if state.panel == Some(hot_id) {
                    state.panel_fired = true;
                    continue;
                }
                if let Some(row) = state
                    .user
                    .iter()
                    .find(|entry| *entry.1 == hot_id)
                    .map(|entry| *entry.0)
                {
                    fired.push(row);
                    continue;
                }
                let Some(id) = state
                    .map
                    .iter()
                    .find(|entry| *entry.1 == hot_id)
                    .map(|entry| entry.0.clone())
                else {
                    continue;
                };
                if let Some(queued) = state.presses.get_mut(&id) {
                    if let Some(done) = queued.pop_front() {
                        done.complete(Ok(json!({ "pressed": true }).to_string()));
                    }
                }
            }
        }
        fired
    }

    fn press(&self, args: &str, done: Done) -> Result<(), String> {
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"]
            .as_str()
            .ok_or_else(|| "缺 id".to_string())?
            .to_string();
        let mut state = self.state.lock().unwrap();
        if !state.map.contains_key(&id) {
            done.complete(Err(format!("热键未注册: {id}")));
            return Ok(());
        }
        state.presses.entry(id).or_default().push_back(done);
        Ok(())
    }

    fn register(&self, args: &str, done: Done) -> Result<(), String> {
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"]
            .as_str()
            .ok_or_else(|| "缺 id".to_string())?
            .to_string();
        let modifiers = modifiers(&parsed["modifiers"])?;
        let key = parsed["key"].as_str().ok_or_else(|| "缺 key".to_string())?;
        let virtual_key = virtual_key(key).ok_or_else(|| format!("不支持的热键 {key:?}"))?;
        let mut state = self.state.lock().unwrap();
        if state.map.contains_key(&id) {
            done.complete(Err(format!("热键已注册: {id}")));
            return Ok(());
        }
        let hot_id = state.take_id();
        match unsafe { RegisterHotKey(None, hot_id, modifiers, virtual_key) } {
            Ok(()) => {
                state.map.insert(id.clone(), hot_id);
                done.complete(Ok(json!({ "registered": true }).to_string()));
            }
            Err(error) => done.complete(Err(format!("热键注册失败: {error}"))),
        }
        Ok(())
    }

    fn unregister(&self, args: &str, done: Done) -> Result<(), String> {
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"]
            .as_str()
            .ok_or_else(|| "缺 id".to_string())?
            .to_string();
        let mut state = self.state.lock().unwrap();
        match state.map.remove(&id) {
            Some(hot_id) => {
                let result = unsafe { UnregisterHotKey(None, hot_id) }.map_err(|e| e.to_string());
                match result {
                    Ok(()) => done.complete(Ok(json!({ "unregistered": true }).to_string())),
                    Err(error) => done.complete(Err(error)),
                }
            }
            None => done.complete(Err(format!("热键未注册: {id}"))),
        }
        Ok(())
    }
}

/// `["ctrl","alt","shift","win"]` → MOD_*;未知修饰键报错。
fn modifiers(value: &serde_json::Value) -> Result<HOT_KEY_MODIFIERS, String> {
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    for modifier in value.as_array().into_iter().flatten() {
        match modifier.as_str() {
            Some("ctrl") => modifiers |= MOD_CONTROL,
            Some("alt") => modifiers |= MOD_ALT,
            Some("shift") => modifiers |= MOD_SHIFT,
            Some("win") => modifiers |= MOD_WIN,
            Some(other) => return Err(format!("不支持的修饰键 {other:?}")),
            None => return Err("修饰键必须是字符串数组".into()),
        }
    }
    Ok(modifiers)
}

/// 单个字符(经 VkKeyScanW)与常用命名键;更多命名键随 T4 的绑定表扩。
fn virtual_key(key: &str) -> Option<u32> {
    if let Some(number) = key
        .strip_prefix('F')
        .and_then(|rest| rest.parse::<u32>().ok())
        && (1..=12).contains(&number)
    {
        return Some(0x70 + number - 1);
    }
    let named = match key {
        "Space" => VK_SPACE,
        "Enter" => VK_RETURN,
        "Tab" => VK_TAB,
        "Backspace" => VK_BACK,
        "Delete" => VK_DELETE,
        "Insert" => VK_INSERT,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "Up" => VK_UP,
        "Down" => VK_DOWN,
        "Left" => VK_LEFT,
        "Right" => VK_RIGHT,
        "Esc" => VK_ESCAPE,
        _ => return single_character(key),
    };
    Some(named.0 as u32)
}

/// 单个字符:ASCII 字母数字直接映射,其余走当前键盘布局。
fn single_character(key: &str) -> Option<u32> {
    let mut chars = key.chars();
    let (first, second) = (chars.next(), chars.next());
    if second.is_some() {
        return None;
    }
    let character = first?;
    if character.is_ascii_alphanumeric() {
        return Some(character.to_ascii_uppercase() as u32);
    }
    if character == ' ' {
        return Some(VK_SPACE.0 as u32);
    }
    let scanned = unsafe { VkKeyScanW(character as u16) };
    if scanned == -1 {
        return None;
    }
    Some((scanned & 0xFF) as u32)
}

/// `Ctrl+Alt+O` → MOD_* 与虚拟键;语法、修饰键数量与保留组合走绑定表的解析。
fn parse_combo(combo: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let hotkey = lueying_extjs::bindings::parse(combo)?;
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    for modifier in &hotkey.modifiers {
        modifiers |= match modifier {
            Modifier::Ctrl => MOD_CONTROL,
            Modifier::Alt => MOD_ALT,
            Modifier::Shift => MOD_SHIFT,
            Modifier::Win => MOD_WIN,
        };
    }
    let key = virtual_key(&hotkey.key).ok_or_else(|| format!("不支持的热键 {combo:?}"))?;
    Ok((modifiers, key))
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod services_tests;

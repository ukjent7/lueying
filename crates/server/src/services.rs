//! Windows 服务提供者:T3 服务缝的平台实现。
//!
//! 提供者从扩展宿主线程被 [`lueying_extjs::Registry::call`] 调进来,慢操作
//! (截图、识别)在专用线程跑完经完成队列回传,不占派发预算。`output` 的
//! TSF 提交链路在 T6 接线,`panel` 的宿主面板在 T4 落地,两者当前回错误
//! 文本,扩展拿到明确原因。
//!
//! 截图文件落在临时目录 `lueying-capture`,文件名带毫秒时间戳;旧文件的
//! 清理随 T8 的预算与可见性一起做。热键在扩展线程上注册,WM_HOTKEY 由
//! [`HotkeyProvider::pump`] 在同一线程收取:扩展 `press` 就地兑现,用户
//! 绑定行号回带给调用方派发。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use lueying_extjs::bindings::Modifier;
use lueying_extjs::{Done, Provider};
use serde_json::json;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPFILEHEADER, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP, VkKeyScanW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MSG, PM_REMOVE, PeekMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WM_HOTKEY,
};
use windows::core::HSTRING;

/// 截屏;`region` 缺省是整个虚拟桌面。在调用线程上执行 GDI 全程。
pub struct CaptureProvider;

impl Provider for CaptureProvider {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        if method != "shot" {
            return Err(format!("capture 没有方法 {method:?}"));
        }
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

/// 文字识别;`lang` 请求的语言不支持时回退用户语言,结果里报实际语言。
pub struct OcrProvider;

impl Provider for OcrProvider {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        if method != "recognize" {
            return Err(format!("ocr 没有方法 {method:?}"));
        }
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let file = parsed["file"]
            .as_str()
            .ok_or_else(|| "缺 file".to_string())?
            .to_string();
        let lang = parsed["lang"].as_str().map(str::to_string);
        std::thread::Builder::new()
            .name("ext-ocr".into())
            .spawn(move || {
                done.complete(ocr_file(&file, lang));
            })
            .map_err(|e| format!("识别线程启动失败: {e}"))?;
        Ok(())
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
    let storage = unsafe { StorageFile::GetFileFromPathAsync(&path) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("GetFileFromPathAsync: {e}"))?;
    let stream = unsafe { storage.OpenAsync(FileAccessMode::Read) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("OpenAsync: {e}"))?;
    let decoder = unsafe { BitmapDecoder::CreateAsync(&stream) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("BitmapDecoder: {e}"))?;
    let bitmap = unsafe { decoder.GetSoftwareBitmapAsync() }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("GetSoftwareBitmapAsync: {e}"))?;
    let engine = match lang {
        Some(tag) => requested_engine(&tag)?,
        None => fallback_engine()?,
    };
    let recognized = unsafe { engine.RecognizeAsync(&bitmap) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("RecognizeAsync: {e}"))?;
    let text = unsafe { recognized.Text() }
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
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        if method != "show" {
            return Err(format!("notify 没有方法 {method:?}"));
        }
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let title = parsed["title"].as_str().unwrap_or_default();
        let body = parsed["body"].as_str().unwrap_or_default();
        println!("扩展通知:{title}:{body}");
        done.complete(Ok("{}".into()));
        Ok(())
    }
}

/// 宿主面板在 T4 落地;当前回错误文本,扩展拿到明确原因。
pub struct PanelProvider;

impl Provider for PanelProvider {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        let _ = args;
        if method != "show" {
            return Err(format!("panel 没有方法 {method:?}"));
        }
        done.complete(Err("面板在 T4 落地".into()));
        Ok(())
    }
}

/// TSF 提交链路在 T6 接线;当前回错误文本,扩展拿到明确原因。
pub struct OutputProvider;

impl Provider for OutputProvider {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        let _ = args;
        if method != "deliver" {
            return Err(format!("output 没有方法 {method:?}"));
        }
        done.complete(Err("output 提交链路在 T6 接线".into()));
        Ok(())
    }
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
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
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

    /// 注销全部热键并丢弃挂起的 press;运行期换扩展时整表重建。
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        let ids = state
            .map
            .values()
            .copied()
            .chain(state.user.values().copied())
            .collect::<Vec<_>>();
        for hot_id in ids {
            let _ = unsafe { UnregisterHotKey(None, hot_id) };
        }
        state.map.clear();
        state.user.clear();
        state.presses.clear();
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
mod services_tests;

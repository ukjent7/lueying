//! 扩展平台验证(任务书第 5 节):V1 OCR、V3 体积、V5 UIA 可编辑、V8 热键。
//!
//! 报告写 `target/ext-verify-report.md`,同时作为一节附进 win-e2e 报告,
//! 由 CI artifact 保留。验证是测量,不是门禁:单项 FAIL 在表里标红,
//! 但不拖红整轮 E2E;真正要拦的链路问题由其余检查负责。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPFILEHEADER, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, FF_DONTCARE, FW_NORMAL, GetDC, OUT_DEFAULT_PRECIS,
    PatBlt, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW, WHITENESS,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, UIA_ButtonControlTypeId, UIA_CONTROLTYPE_ID,
    UIA_EditControlTypeId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, RegisterHotKey, SetFocus, UnregisterHotKey, VK_DELETE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, ES_READONLY, RegisterClassW, SW_SHOWNORMAL,
    ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW,
    WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

/// 一条验证结果。
enum Verdict {
    /// 满足预期。
    Pass,
    /// 与预期不符;要修或要改设计。
    Fail,
    /// 测量与结论;没有通过/失败语义。
    Note,
}

type Row = (&'static str, &'static str, Verdict, String);

/// 跑全部验证,返回报告小节并落盘独立 artifact。
pub(super) fn section() -> String {
    let mut rows: Vec<Row> = Vec::new();
    v3_sizes(&mut rows);
    v8_hotkey(&mut rows);
    v5_uia(&mut rows);
    v1_ocr(&mut rows);
    deferred(&mut rows);

    let mut text = String::from(
        "## 扩展平台验证(任务书 V1–V8)\n\n| 项 | 检查 | 结果 | 说明 |\n|---|---|---|---|\n",
    );
    for (id, name, verdict, note) in &rows {
        let flag = match verdict {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Note => "NOTE",
        };
        let note = note.replace('|', "\\|").replace('\n', " ");
        text.push_str(&format!("| {id} | {name} | {flag} | {note} |\n"));
    }
    let _ = std::fs::create_dir_all("target");
    let _ = std::fs::write("target/ext-verify-report.md", &text);
    text
}

/// V3(部分):二进制体积。帧级 GPU/CPU 测量要等候选窗渲染核心能独立驱动,
/// 挂到 T17 之前;体积先给 Skia 裁剪评估一个基线。
fn v3_sizes(rows: &mut Vec<Row>) {
    let mut parts = Vec::new();
    for name in [
        "lueying-server.exe",
        "lueying-settings.exe",
        "lueying-update.exe",
        "lueying-user.exe",
        "lueying_tsf.dll",
        "lueying_tsf32.dll",
    ] {
        let path = Path::new("target/release").join(name);
        match std::fs::metadata(&path) {
            Ok(meta) => parts.push(format!("{name}={:.1}MiB", meta.len() as f64 / 1048576.0)),
            Err(_) => parts.push(format!("{name}=缺")),
        }
    }
    rows.push((
        "V3",
        "二进制体积(Skia 裁剪评估基线)",
        Verdict::Note,
        parts.join(", "),
    ));
    rows.push((
        "V3",
        "候选窗 GPU/CPU 常驻内存与首帧",
        Verdict::Note,
        "帧级测量需要候选窗渲染核心可独立驱动(基准模式);挂到 T17 前补。".to_string(),
    ));
}

/// V8:RegisterHotKey 的注册、冲突与保留键;管理员前台场景在本环境不可复现。
fn v8_hotkey(rows: &mut Vec<Row>) {
    let first = unsafe { RegisterHotKey(None, 1, MOD_CONTROL | MOD_ALT, 0x4B) };
    let duplicate = unsafe { RegisterHotKey(None, 1, MOD_CONTROL | MOD_ALT, 0x4B) };
    let released = unsafe { UnregisterHotKey(None, 1) };
    let reserved = unsafe { RegisterHotKey(None, 2, MOD_CONTROL | MOD_ALT, VK_DELETE.0 as u32) };
    let _ = unsafe { UnregisterHotKey(None, 2) };
    let ok = first.is_ok() && duplicate.is_err() && released.is_ok();
    rows.push((
        "V8",
        "RegisterHotKey 注册/冲突/注销",
        if ok { Verdict::Pass } else { Verdict::Fail },
        format!("注册={first:?} 重复={duplicate:?} 注销={released:?}"),
    ));
    rows.push((
        "V8",
        "保留键 Ctrl+Alt+Del",
        Verdict::Note,
        format!("RegisterHotKey={reserved:?}(预期失败;具体错误码随系统而变)"),
    ));
    rows.push((
        "V8",
        "管理员前台窗口的热键",
        Verdict::Note,
        "CI runner 全程高权限,普通进程注册的热键在特权前台是否触发无法在本环境复现;留待带降权子进程的后续验证。".to_string(),
    ));
}

/// V5:无 TSF 会话时,UIA 能否回答「焦点是否可编辑」。
fn v5_uia(rows: &mut Vec<Row>) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(uia_focus_probe))
        .unwrap_or_else(|_| Err("UIA 探测 panic".into()));
    match outcome {
        Ok((edit, read_only, button)) => {
            let ok = edit == UIA_EditControlTypeId
                && read_only == UIA_EditControlTypeId
                && button == UIA_ButtonControlTypeId;
            rows.push((
                "V5",
                "UIA 判断焦点是否可编辑",
                if ok { Verdict::Pass } else { Verdict::Fail },
                format!(
                    "EDIT→{edit:?} 只读EDIT→{read_only:?} BUTTON→{button:?};只读态控件类型仍是 Edit,可编辑判定要靠 IsReadOnly/ValuePattern 补充——控件类型只够分「可输入/不可输入」大类。"
                ),
            ));
        }
        Err(error) => rows.push(("V5", "UIA 判断焦点是否可编辑", Verdict::Fail, error)),
    }
}

/// windows-rs 里的 DefWindowProcW 是 Rust 函数,作 WNDPROC 需要包一层 system ABI。
unsafe extern "system" fn proc_def(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 建一个带 EDIT/只读 EDIT/BUTTON 的小窗口,逐个 SetFocus 后读 UIA 控件类型。
/// COM 与 UIA 客户端都可用:外层 E2E 已把本线程初始化成 STA。
fn uia_focus_probe() -> Result<(UIA_CONTROLTYPE_ID, UIA_CONTROLTYPE_ID, UIA_CONTROLTYPE_ID), String>
{
    let class_name: Vec<u16> = "lueying-verify-uia\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(proc_def),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let parent = unsafe {
        if RegisterClassW(&wc) == 0 {
            return Err("RegisterClassW 失败".into());
        }
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            320,
            200,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("建验证窗口失败: {e}"))?;
    let edit = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            10,
            10,
            120,
            26,
            Some(parent),
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("建 EDIT 失败: {e}"))?;
    let read_only = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_READONLY as u32),
            10,
            46,
            120,
            26,
            Some(parent),
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("建只读 EDIT 失败: {e}"))?;
    let button = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            10,
            82,
            120,
            30,
            Some(parent),
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("建 BUTTON 失败: {e}"))?;
    unsafe {
        let _ = ShowWindow(parent, SW_SHOWNORMAL);
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("CoCreateInstance(UIAutomation): {e}"))?;
        let kind_of = |target| -> Result<UIA_CONTROLTYPE_ID, String> {
            let _ = SetFocus(Some(target));
            let focused = automation
                .GetFocusedElement()
                .map_err(|e| format!("GetFocusedElement: {e}"))?;
            focused
                .CurrentControlType()
                .map_err(|e| format!("CurrentControlType: {e}"))
        };
        let edit_kind = kind_of(edit)?;
        let read_only_kind = kind_of(read_only)?;
        let button_kind = kind_of(button)?;
        let _ = DestroyWindow(parent);
        Ok((edit_kind, read_only_kind, button_kind))
    }
}

/// V1:Windows.Media.Ocr。独立 MTA 线程里跑,避免占 E2E 的 STA。
/// 普通 push 测「语言包缺失时的行为」;workflow_dispatch 会先装 zh-CN
/// OCR 能力,届时同一段代码给出中文识别质量。
fn v1_ocr(rows: &mut Vec<Row>) {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("verify-ocr".into())
        .spawn(move || {
            let _ = tx.send(ocr_check());
        });
    let outcome = if spawned.is_err() {
        Err("OCR 线程启动失败".into())
    } else {
        rx.recv_timeout(Duration::from_secs(120))
            .unwrap_or_else(|_| Err("OCR 线程 120 秒未返回".into()))
    };
    match outcome {
        Ok((verdict, detail)) => rows.push(("V1", "Windows.Media.Ocr", verdict, detail)),
        Err(error) => rows.push(("V1", "Windows.Media.Ocr", Verdict::Fail, error)),
    }
}

/// OCR 检查体;在自带 COM 初始化的线程上运行。
fn ocr_check() -> Result<(Verdict, String), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;
    }
    let outcome = ocr_inner();
    unsafe { CoUninitialize() };
    outcome
}

fn ocr_inner() -> Result<(Verdict, String), String> {
    let languages = unsafe { OcrEngine::AvailableRecognizerLanguages() }
        .map_err(|e| format!("查询可用识别语言失败: {e}"))?;
    let count = unsafe { languages.Size() }.map_err(|e| format!("识别语言数量: {e}"))?;
    let mut tags = Vec::new();
    for index in 0..count {
        let tag = unsafe { languages.GetAt(index) }
            .and_then(|language| unsafe { language.LanguageTag() })
            .map_err(|e| format!("识别语言 {index}: {e}"))?;
        tags.push(tag.to_string());
    }
    let zh_language = unsafe { Language::CreateLanguage(&HSTRING::from("zh-Hans-CN")) }
        .map_err(|e| format!("建 zh-Hans-CN 语言对象失败: {e}"))?;
    let zh_supported = unsafe { OcrEngine::IsLanguageSupported(&zh_language) }
        .map_err(|e| format!("IsLanguageSupported: {e}"))?;
    let engine = match unsafe { OcrEngine::TryCreateFromLanguage(&zh_language) } {
        Ok(engine) => Some(engine),
        Err(error) if zh_supported => {
            return Err(format!("zh 引擎创建失败但语言受支持: {error}"));
        }
        Err(_) => unsafe { OcrEngine::TryCreateFromUserProfileLanguages() }.ok(),
    };
    let Some(engine) = engine else {
        return Ok((
            Verdict::Note,
            format!(
                "没有可用 OCR 引擎(可用语言 {tags:?})。语言包缺失时扩展必须走这条路径的兜底:服务缝里降级或提示安装语言包。"
            ),
        ));
    };
    let sample = write_sample_bitmap()?;
    let path = HSTRING::from(sample.to_str().unwrap_or_default());
    let file = unsafe { StorageFile::GetFileFromPathAsync(&path) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("GetFileFromPathAsync: {e}"))?;
    let stream = unsafe { file.OpenAsync(FileAccessMode::Read) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("OpenAsync: {e}"))?;
    let decoder = unsafe { BitmapDecoder::CreateAsync(&stream) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("BitmapDecoder: {e}"))?;
    let bitmap = unsafe { decoder.GetSoftwareBitmapAsync() }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("GetSoftwareBitmapAsync: {e}"))?;
    let recognized = unsafe { engine.RecognizeAsync(&bitmap) }
        .and_then(|operation| operation.join())
        .map_err(|e| format!("RecognizeAsync: {e}"))?;
    let text = unsafe { recognized.Text() }
        .map_err(|e| format!("OcrResult.Text: {e}"))?
        .to_string();
    let compact = text.split_whitespace().collect::<Vec<_>>().join("");
    let matched = ["掠影", "输入法", "扩展", "验证", "2026"]
        .iter()
        .filter(|needle| compact.contains(*needle))
        .count();
    let verdict = if zh_supported && matched >= 3 {
        Verdict::Pass
    } else {
        Verdict::Note
    };
    let note = if zh_supported {
        format!(
            "zh-Hans-CN 引擎识别:样本「掠影输入法扩展验证 2026年10月30日 OCR 样张」→「{text}」;命中关键词 {matched}/5。"
        )
    } else {
        format!(
            "zh 语言包缺失(可用语言 {tags:?}),走了用户语言回退;这是语言包缺失时输入法应有的行为样本。识别文本:「{text}」。手动触发 CI 会先装 zh-CN 能力再复测质量。"
        )
    };
    Ok((verdict, note))
}

/// 用 GDI 把中文样张画进 24 位 BMP:OCR 只认位图,这里不引入 Skia 依赖。
fn write_sample_bitmap() -> Result<PathBuf, String> {
    const WIDTH: i32 = 800;
    const HEIGHT: i32 = 200;
    const STRIDE: usize = (WIDTH as usize * 3 + 3) / 4 * 4;

    let pixels = unsafe {
        let hdc = GetDC(None);
        let memory = CreateCompatibleDC(Some(hdc));
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = WIDTH;
        bmi.bmiHeader.biHeight = HEIGHT;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 24;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let section = CreateDIBSection(Some(memory), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        let previous = SelectObject(memory, section.into());
        let _ = PatBlt(memory, 0, 0, WIDTH, HEIGHT, WHITENESS);
        let font = CreateFontW(
            -72,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Microsoft YaHei"),
        );
        let previous_font = SelectObject(memory, font.into());
        let _ = SetBkMode(memory, TRANSPARENT);
        let _ = SetTextColor(memory, COLORREF(0));
        let line1: Vec<u16> = "掠影输入法扩展验证\0".encode_utf16().collect();
        let line2: Vec<u16> = "2026年10月30日 OCR 样张\0".encode_utf16().collect();
        let _ = TextOutW(memory, 24, 24, &line1);
        let _ = TextOutW(memory, 24, 112, &line2);
        SelectObject(memory, previous_font);
        SelectObject(memory, previous);

        let pixels =
            std::slice::from_raw_parts(bits as *const u8, STRIDE * HEIGHT as usize).to_vec();
        DeleteObject(font.into());
        DeleteObject(section.into());
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(None, hdc);
        pixels
    };

    let mut file_header = BITMAPFILEHEADER::default();
    file_header.bfType = 0x4d42;
    file_header.bfSize = (54 + pixels.len()) as u32;
    file_header.bfOffBits = 54;
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&file_header as *const BITMAPFILEHEADER) as *const u8,
            std::mem::size_of::<BITMAPFILEHEADER>(),
        )
    };
    let info_bytes = unsafe {
        std::slice::from_raw_parts(
            (&BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: WIDTH,
                biHeight: HEIGHT,
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                ..Default::default()
            } as *const BITMAPINFOHEADER) as *const u8,
            std::mem::size_of::<BITMAPINFOHEADER>(),
        )
    };
    let mut bytes = Vec::with_capacity(pixels.len() + 54);
    bytes.extend_from_slice(header_bytes);
    bytes.extend_from_slice(info_bytes);
    bytes.extend_from_slice(&pixels);

    let path = std::env::temp_dir().join("lueying-ocr-sample.bmp");
    std::fs::write(&path, bytes).map_err(|e| format!("写样张失败: {e}"))?;
    Ok(path)
}

/// 等实现补齐的验证项;先记方法,避免到时候凭感觉。
fn deferred(rows: &mut Vec<Row>) {
    rows.push((
        "V2",
        "异步结果经 TSF 轮询上屏",
        Verdict::Note,
        "依赖 T3 output.deliver 的异步链路;poll.rs 的 40ms 停顿轮询已承载 25M 重排的异步写回(先例),T3 落地后补全链路 E2E。"
            .to_string(),
    ));
    rows.push((
        "V4",
        "命令面板归还焦点后上屏",
        Verdict::Note,
        "面板已实现,`--panel-selftest` 覆盖记录/归还前台窗口、拼音搜索、方向键、Enter 与 Esc;`output.deliver` 已接线为 Unicode 注入,win-e2e 在 RichEdit 上跑通热键 → 截图 → 识别 → 上屏。Electron 与自绘控件需要可下载的测试应用,留待手动实测。"
            .to_string(),
    ));
    rows.push((
        "V6",
        "rquickjs Promise 与作业队列",
        Verdict::Note,
        "已在 crates/e2e/src/extjs.rs 异步命令 E2E 覆盖:同步 Runtime + interrupt handler + execute_pending_job 的步进模型可行,单轮 256 步上限,死循环作业被派发预算打断。"
            .to_string(),
    ));
    rows.push((
        "V7",
        "AccessKit 集成 Skia 自绘窗口",
        Verdict::Note,
        "依赖 T9;方案:设置窗接 SubclassingAdapter 回答 WM_GETOBJECT,win-e2e 断言 UIA 树,短语编辑的文本输入方案一并验证。"
            .to_string(),
    ));
}

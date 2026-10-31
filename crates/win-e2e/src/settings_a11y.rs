//! 设置窗无障碍 E2E:用 UIA 当读屏软件,读 AccessKit 树,再按「编辑」
//! 打开短语编辑器,确认标准 EDIT 文本控件真的存在。
//!
//! 这是 V7 的落地检查:只断言树里有控件还不够,动作必须真的走通
//! UIA → AccessKit → 窗口线程 → 打开编辑器这条链。

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
    IUIAutomationValuePattern, TreeScope_Descendants, UIA_ButtonControlTypeId, UIA_CONTROLTYPE_ID,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_GroupControlTypeId, UIA_InvokePatternId,
    UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
use windows::core::{BSTR, PCWSTR, w};

/// 用 UIA 写进编辑器的短语;保存后应当出现在 phrases.txt 里。
const PHRASE: &str = "E2E 短语\te2e\t100";

/// 启动设置窗,断言 UIA 树与短语编辑器的文本输入。
pub fn check(exe_dir: &Path, data: &Path) -> Result<String, String> {
    let exe = exe_dir.join("lueying-settings.exe");
    if !exe.exists() {
        return Err(format!("没有 {}", exe.display()));
    }
    let mut child = Command::new(&exe)
        .env("LOCALAPPDATA", data)
        .env("APPDATA", data)
        .spawn()
        .map_err(|e| format!("启动设置窗失败: {e}"))?;
    let outcome = check_ui(&mut child, data);
    close_window("lueying-settings-edit");
    close_window("lueying-settings");
    wait_or_kill(&mut child);
    let lines = outcome?;
    Ok(format!("## 设置窗无障碍(UIA)\n\n{}\n", lines.join("\n")))
}

fn check_ui(child: &mut Child, data: &Path) -> Result<Vec<String>, String> {
    let settings = wait_window(w!("lueying-settings"), Duration::from_secs(20), child)?;
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("CoCreateInstance(UIAutomation): {e}"))?;
    let root = unsafe { automation.ElementFromHandle(settings) }
        .map_err(|e| format!("ElementFromHandle(设置窗): {e}"))?;
    let name = text(&root);
    if name != "掠影设置" {
        return Err(format!("设置窗 UIA 名称是 `{name}`"));
    }
    let elements = descendants(&automation, &root)?;
    let mut lines = vec![format!(
        "设置窗 UIA 名称 `{name}`,节点 {} 个",
        elements.len()
    )];
    for required in ["保存", "关闭", "输入", "辅助码", "检查更新"] {
        if !elements.iter().any(|element| text(element) == required) {
            return Err(format!("UIA 树里找不到「{required}」"));
        }
    }
    lines.push("UIA 树包含保存、关闭、输入、辅助码与检查更新".to_string());
    let phrase_group = elements
        .iter()
        .find(|element| {
            text(element) == "自定义短语" && kind(element) == Some(UIA_GroupControlTypeId)
        })
        .ok_or("UIA 树里找不到「自定义短语」分组")?;
    let phrase_row = descendants(&automation, phrase_group)?;
    let edit_button = phrase_row
        .iter()
        .find(|element| text(element) == "编辑" && kind(element) == Some(UIA_ButtonControlTypeId))
        .ok_or("「自定义短语」分组里找不到编辑按钮")?;
    let invoke: IUIAutomationInvokePattern =
        unsafe { edit_button.GetCurrentPatternAs(UIA_InvokePatternId) }
            .map_err(|e| format!("编辑按钮没有 InvokePattern: {e}"))?;
    unsafe { invoke.Invoke() }.map_err(|e| format!("UIA 调用编辑按钮失败: {e}"))?;
    lines.push("UIA 调用「编辑」按钮成功".to_string());

    let editor = wait_window(w!("lueying-settings-edit"), Duration::from_secs(5), child)?;
    let editor_root = unsafe { automation.ElementFromHandle(editor) }
        .map_err(|e| format!("ElementFromHandle(短语编辑器): {e}"))?;
    let editor_name = text(&editor_root);
    if editor_name != "编辑自定义短语" {
        return Err(format!("短语编辑器 UIA 名称是 `{editor_name}`"));
    }
    let controls = descendants(&automation, &editor_root)?;
    let edit = controls
        .iter()
        .find(|element| {
            // 多行 EDIT 在 UIA 代理下是 Document 控件类型;单行才是 Edit。
            matches!(
                kind(element),
                Some(UIA_EditControlTypeId) | Some(UIA_DocumentControlTypeId)
            )
        })
        .ok_or("短语编辑器里没有 EDIT 文本控件")?;
    let value: IUIAutomationValuePattern = unsafe { edit.GetCurrentPatternAs(UIA_ValuePatternId) }
        .map_err(|e| format!("EDIT 没有 ValuePattern: {e}"))?;
    unsafe { value.SetValue(&BSTR::from(PHRASE)) }.map_err(|e| format!("UIA 写入短语失败: {e}"))?;
    let save = controls
        .iter()
        .find(|element| text(element) == "保存" && kind(element) == Some(UIA_ButtonControlTypeId))
        .ok_or("短语编辑器里找不到保存按钮")?;
    let invoke: IUIAutomationInvokePattern =
        unsafe { save.GetCurrentPatternAs(UIA_InvokePatternId) }
            .map_err(|e| format!("保存按钮没有 InvokePattern: {e}"))?;
    unsafe { invoke.Invoke() }.map_err(|e| format!("UIA 调用保存失败: {e}"))?;
    let path = data.join("lueying").join("phrases.txt");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(content) = std::fs::read_to_string(&path)
            && content.contains(PHRASE)
        {
            break;
        }
        if Instant::now() > deadline {
            return Err(format!("短语没有写回 {}", path.display()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    lines.push(format!(
        "短语编辑器 UIA 名称 `{editor_name}`,EDIT 控件可写,保存后 phrases.txt 已更新"
    ));
    Ok(lines)
}

fn wait_window(class: PCWSTR, timeout: Duration, child: &mut Child) -> Result<HWND, String> {
    let start = Instant::now();
    loop {
        if let Ok(hwnd) = unsafe { FindWindowW(class, PCWSTR::null()) } {
            return Ok(hwnd);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("设置窗提前退出: {status}"));
        }
        if start.elapsed() > timeout {
            return Err(format!("等待窗口超时: {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn descendants(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
) -> Result<Vec<IUIAutomationElement>, String> {
    let condition = unsafe { automation.CreateTrueCondition() }
        .map_err(|e| format!("CreateTrueCondition: {e}"))?;
    let array = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
        .map_err(|e| format!("FindAll: {e}"))?;
    let count = unsafe { array.Length() }.map_err(|e| format!("UIA 节点数: {e}"))?;
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        if let Ok(element) = unsafe { array.GetElement(index) } {
            out.push(element);
        }
    }
    Ok(out)
}

fn text(element: &IUIAutomationElement) -> String {
    unsafe { element.CurrentName() }
        .map(|name| name.to_string())
        .unwrap_or_default()
}

fn kind(element: &IUIAutomationElement) -> Option<UIA_CONTROLTYPE_ID> {
    unsafe { element.CurrentControlType() }.ok()
}

fn close_window(class: &str) {
    let name: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
    if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR(name.as_ptr()), PCWSTR::null()) } {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn wait_or_kill(child: &mut Child) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

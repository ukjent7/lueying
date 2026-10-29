//! TSF 宿主链路的断言:候选 UI、触屏布局、落定、按音节编辑、全角与保留键。
//!
//! 这些函数只读取 TSF/EDIT 的状态并返回给 `real_run` 汇总,不负责注册或清理。

use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElementBehavior,
    ITfFnGetPreferredTouchKeyboardLayout, ITfIntegratableCandidateListUIElement, ITfUIElementMgr,
    STYLE_ACTIVE_SELECTION, TKBL_OPT_SIMPLIFIED_CHINESE_PINYIN, TKBLT_OPTIMIZED, TKBLayoutType,
};

use super::*;

/// TSF 的候选 UI 元素必须能回答宿主/读屏软件:至少有一条候选。
pub(super) fn check_candidate_ui(
    thread_mgr: &ITfThreadMgr,
    events: &Rc<RefCell<UiEvents>>,
) -> Result<String, String> {
    let (began, id, updates) = {
        let events = events.borrow();
        (events.began, events.id, events.updates)
    };
    if !began {
        return Err("TSF 没有登记候选 UI 元素".into());
    }
    if updates == 0 {
        return Err("候选 UI 元素没有收到 UpdateUIElement".into());
    }
    let mgr: ITfUIElementMgr = thread_mgr
        .cast()
        .map_err(|e| format!("cast ITfUIElementMgr: {e}"))?;
    let element =
        unsafe { mgr.GetUIElement(id) }.map_err(|e| format!("GetUIElement({id}): {e}"))?;
    let list: ITfCandidateListUIElement = element
        .cast()
        .map_err(|e| format!("cast ITfCandidateListUIElement: {e}"))?;
    // SetSelection/Finalize/Abort 在 Behavior 接口上,与列表接口分开查询。
    let behavior: ITfCandidateListUIElementBehavior = element
        .cast()
        .map_err(|e| format!("cast ITfCandidateListUIElementBehavior: {e}"))?;
    let count = unsafe { list.GetCount() }.map_err(|e| format!("GetCount: {e}"))?;
    if count == 0 {
        return Err("候选 UI 元素返回 0 条候选".into());
    }
    let first = unsafe { list.GetString(0) }
        .map_err(|e| format!("GetString(0): {e}"))?
        .to_string();
    if first.is_empty() {
        return Err("候选 UI 元素首条候选为空".into());
    }
    // Windows 搜索框一类宿主会 QI 集成候选接口;内部列表必须声明
    // 高亮即待上屏项,并允许宿主画候选编号。
    let integrated: ITfIntegratableCandidateListUIElement = element
        .cast()
        .map_err(|e| format!("cast ITfIntegratableCandidateListUIElement: {e}"))?;
    let style =
        unsafe { integrated.GetSelectionStyle() }.map_err(|e| format!("GetSelectionStyle: {e}"))?;
    let numbers = unsafe { integrated.ShowCandidateNumbers() }
        .map_err(|e| format!("ShowCandidateNumbers: {e}"))?;
    if style != STYLE_ACTIVE_SELECTION || !numbers.as_bool() {
        return Err(format!(
            "集成候选接口返回 style={style:?} numbers={numbers:?}"
        ));
    }
    // 宿主自绘时要能翻页:页索引必须可读,SetSelection 换页后当前页要跟随。
    let mut page_starts = [0u32; 8];
    let mut pages = 0u32;
    unsafe { list.GetPageIndex(&mut page_starts, &mut pages) }
        .map_err(|e| format!("GetPageIndex: {e}"))?;
    if pages == 0 {
        return Err("候选 UI 元素没有页索引".into());
    }
    let mut paging = String::new();
    if pages > 1 {
        unsafe { behavior.SetSelection(page_starts[1]) }
            .map_err(|e| format!("SetSelection(第二页): {e}"))?;
        let current =
            unsafe { list.GetCurrentPage() }.map_err(|e| format!("GetCurrentPage: {e}"))?;
        let selection = unsafe { list.GetSelection() }.map_err(|e| format!("GetSelection: {e}"))?;
        if current != 1 || selection != page_starts[1] {
            return Err(format!(
                "宿主选中第二页首项后 current={current} selection={selection},应为 1/{}",
                page_starts[1]
            ));
        }
        unsafe { behavior.SetSelection(0) }.map_err(|e| format!("SetSelection(复位): {e}"))?;
        let current =
            unsafe { list.GetCurrentPage() }.map_err(|e| format!("GetCurrentPage(复位): {e}"))?;
        if current != 0 {
            return Err(format!("复位后当前页是 {current},应为 0"));
        }
        paging = format!(",{pages} 页");
    }
    Ok(format!("候选 UI 元素:{count} 条{paging},首条 `{first}`"))
}

/// 触屏键盘经 Function Provider 查询首选布局;返回简体拼音优化布局。
pub(super) fn check_touch_layout(thread_mgr: &ITfThreadMgr) -> Result<String, String> {
    let provider = unsafe { thread_mgr.GetFunctionProvider(&CLSID_IME) }
        .map_err(|e| format!("GetFunctionProvider: {e}"))?;
    let unknown = unsafe {
        provider.GetFunction(std::ptr::null(), &ITfFnGetPreferredTouchKeyboardLayout::IID)
    }
    .map_err(|e| format!("GetFunction(触屏布局): {e}"))?;
    let layout: ITfFnGetPreferredTouchKeyboardLayout = unknown
        .cast()
        .map_err(|e| format!("cast ITfFnGetPreferredTouchKeyboardLayout: {e}"))?;
    let mut kind = TKBLayoutType::default();
    let mut id = 0u16;
    unsafe { layout.GetLayout(&mut kind, (&mut id) as *mut u16 as *const u16) }
        .map_err(|e| format!("GetLayout: {e}"))?;
    if kind != TKBLT_OPTIMIZED || id != TKBL_OPT_SIMPLIFIED_CHINESE_PINYIN as u16 {
        return Err(format!(
            "触屏布局 kind={kind:?} id={id},应为简体拼音优化布局"
        ));
    }
    Ok(format!("## 触屏键盘\n\nPASS,layout={id}"))
}

/// 搜索框按 Enter 时宿主调用原样落定:提交眼前显示的汉字,组合与 UI 元素结束。
pub(super) fn check_finalize(
    edit: HWND,
    thread_mgr: &ITfThreadMgr,
    events: &Rc<RefCell<UiEvents>>,
) -> Result<String, String> {
    *events.borrow_mut() = UiEvents::default();
    type_text("nihao")?;
    pump_for(Duration::from_millis(300));
    let (began, id) = {
        let events = events.borrow();
        (events.began, events.id)
    };
    if !began {
        return Err("原样落定前没有登记候选 UI 元素".into());
    }
    let mgr: ITfUIElementMgr = thread_mgr
        .cast()
        .map_err(|e| format!("cast ITfUIElementMgr: {e}"))?;
    let element = unsafe { mgr.GetUIElement(id) }.map_err(|e| format!("GetUIElement: {e}"))?;
    let integrated: ITfIntegratableCandidateListUIElement = element
        .cast()
        .map_err(|e| format!("cast ITfIntegratableCandidateListUIElement: {e}"))?;
    unsafe { integrated.FinalizeExactCompositionString() }
        .map_err(|e| format!("FinalizeExactCompositionString: {e}"))?;
    let deadline = Instant::now() + Duration::from_millis(800);
    while composing(thread_mgr)? {
        if Instant::now() >= deadline {
            return Err("原样落定后组合没有结束".into());
        }
        pump_for(Duration::from_millis(5));
    }
    pump_for(Duration::from_millis(100));
    let text = edit_text(edit);
    // 上一轮延迟测试已上屏「你好」;原样落定提交当前显示,再追加一次。
    if !text.ends_with("你好你好") {
        return Err(format!("原样落定后文本 `{text}` 不以 你好你好 结尾"));
    }
    if !events.borrow().ended {
        return Err("原样落定后候选 UI 元素没有结束".into());
    }
    Ok(format!("## 宿主原样落定\n\nPASS,文本 `{text}`"))
}

/// 组合中 Ctrl+Backspace 删除一个音节,再空格上屏剩下的候选。
pub(super) fn check_segment_edit(edit: HWND, thread_mgr: &ITfThreadMgr) -> Result<String, String> {
    type_text("nihao")?;
    pump_for(Duration::from_millis(250));
    chord(0x11, 0x08)?; // Ctrl+Backspace
    pump_for(Duration::from_millis(250));
    tap(0x20)?; // 空格
    pump_for(Duration::from_millis(500));
    if composing(thread_mgr)? {
        return Err("Ctrl+Backspace 后组合没有在空格上屏时结束".into());
    }
    let text = edit_text(edit);
    if !text.ends_with('你') {
        return Err(format!("Ctrl+Backspace 后文本 `{text}` 不以「你」结尾"));
    }
    // Ctrl+Left 把组合光标移到「ni|hao」的音节边界,再按 Ctrl+Backspace
    // 只删掉前一个音节;若移动没生效会整段删成「你」,文本就不会以「你好」结尾。
    type_text("nihao")?;
    pump_for(Duration::from_millis(250));
    chord(0x11, 0x25)?; // Ctrl+Left
    pump_for(Duration::from_millis(250));
    chord(0x11, 0x08)?; // Ctrl+Backspace
    pump_for(Duration::from_millis(250));
    tap(0x20)?;
    pump_for(Duration::from_millis(500));
    if composing(thread_mgr)? {
        return Err("Ctrl+Left 后组合没有在空格上屏时结束".into());
    }
    let text = edit_text(edit);
    if !text.ends_with("你好") {
        return Err(format!(
            "Ctrl+Left 按音节编辑后文本 `{text}` 不以「你好」结尾"
        ));
    }
    Ok(format!("## 组合按音节编辑\n\nPASS,文本 `{text}`"))
}

pub(super) fn check_fullwidth(edit: HWND) -> Result<String, String> {
    // 中文态空闲字符:全角数字与全角空格,然后关掉。
    chord(0x10, 0x20)?; // Shift+Space
    pump_for(Duration::from_millis(250));
    tap(0x31)?; // 1
    pump_for(Duration::from_millis(250));
    tap(0x20)?; // 空格
    pump_for(Duration::from_millis(250));
    chord(0x10, 0x20)?; // 关全角
    pump_for(Duration::from_millis(250));
    // 英文态全角字母:Shift 单击切英文,再开全角。
    tap(0x10)?;
    pump_for(Duration::from_millis(250));
    chord(0x10, 0x20)?;
    pump_for(Duration::from_millis(250));
    type_text("a")?;
    pump_for(Duration::from_millis(250));
    chord(0x10, 0x20)?; // 关全角
    pump_for(Duration::from_millis(250));
    tap(0x10)?; // 回中文,避免影响后续测试
    pump_for(Duration::from_millis(300));
    let text = edit_text(edit);
    if !text.ends_with("１　ａ") {
        return Err(format!("全角输入后文本 `{text}` 不以「１　ａ」结尾"));
    }
    Ok(format!("## 全角模式\n\nPASS,文本 `{text}`"))
}

/// Skia 渲染管线自检:Server 启动时按 `LUEYING_SKIA_SELFTEST` 出帧并读回
/// 像素,结论写进日志;这里读日志断言。光栅路径的像素断言不过就红;
/// GPU 路径在子进程里跑,没有硬件适配器或驱动异常时不可用——不可用不算失败,
/// 但原因要带进报告,免得「GPU 没验过」被悄悄咽下去。
pub(super) fn check_skia_selftest() -> Result<String, String> {
    let text = std::fs::read_to_string(server_log()).unwrap_or_default();
    if let Some(line) = text.lines().find(|line| line.contains("SKIA 自检失败")) {
        return Err(format!("Skia 自检失败:{line}"));
    }
    let Some(line) = text.lines().find(|line| line.contains("SKIA 自检通过")) else {
        return Err("Server 日志里没有 Skia 自检记录".into());
    };
    let gpu = text
        .lines()
        .find(|line| {
            line.contains("SKIA GPU 自检通过")
                || line.contains("SKIA GPU 自检不可用")
                || line.contains("SKIA GPU 自检失败")
        })
        .unwrap_or("无");
    // CPU 兜底(没有硬件 D3D12 的机器上候选窗走这条路):结论带进报告,
    // 失败不拖垮整轮 E2E——分层窗口在某些无桌面会话里本来就建不出来。
    let cpu = text
        .lines()
        .find(|line| {
            line.contains("SKIA CPU 自检通过") || line.contains("SKIA CPU 自检失败")
        })
        .unwrap_or("无");
    Ok(format!(
        "## Skia 渲染管线\n\nPASS,{line}\n\nGPU:{gpu}\n\nCPU:{cpu}"
    ))
}

/// Ctrl+. 是 TSF 保留键;切换后 Server 应把 chinese_punct 写进 config.ini。
pub(super) fn check_preserved_punct(data: &std::path::Path) -> Result<(), String> {
    const VK_CONTROL: u16 = 0x11;
    const VK_OEM_PERIOD: u16 = 0xBE;
    let config = data.join("lueying").join("config.ini");
    let expect = |want: &str| -> Result<(), String> {
        // 配置在 Server 线程写完才改名,给 CI 磁盘留足时间。
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            let text = std::fs::read_to_string(&config).unwrap_or_default();
            if text.contains(&format!("chinese_punct={want}")) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} 不含 chinese_punct={want}: `{text}`",
                    config.display()
                ));
            }
            pump_for(Duration::from_millis(25));
        }
    };
    chord(VK_CONTROL, VK_OEM_PERIOD)?;
    expect("false")?;
    chord(VK_CONTROL, VK_OEM_PERIOD)?;
    expect("true")?;
    Ok(())
}

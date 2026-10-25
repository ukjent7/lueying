//! 中/英模式与系统状态同步:写「转换模式」「输入法开/关」两条 compartment,
//! 让 Win11 输入指示器、Ctrl+Space 和任务栏菜单与输入法保持一致;反向变化
//! 经 [`ITfCompartmentEventSink`] 回到 Server,成为全局模式。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use lueying_protocol::Request;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::TextServices::{
    GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, GUID_COMPARTMENT_KEYBOARD_OPENCLOSE,
    ITfCompartment, ITfCompartmentEventSink, ITfCompartmentEventSink_Impl, ITfCompartmentMgr,
    ITfSource, ITfThreadMgr, TF_CONVERSIONMODE_NATIVE,
};
use windows::core::{GUID, Interface, Result, implement};

use crate::log;
use crate::service::{Shared, SharedClient, apply_reply, request_with};

const NATIVE: i32 = TF_CONVERSIONMODE_NATIVE as i32;

pub(crate) type Advice = Vec<(ITfSource, u32)>;

/// 监听系统侧的模式 compartment;一条挂不上只记日志,另一条照常。
pub(crate) fn advise(
    thread_mgr: &ITfThreadMgr,
    shared: Rc<Shared>,
    client: SharedClient,
) -> Advice {
    let sink: ITfCompartmentEventSink = ModeSink {
        thread_mgr: thread_mgr.clone(),
        shared,
        client,
    }
    .into();
    let mut advice = Vec::new();
    for (name, compartment) in [
        ("转换模式", conversion_compartment(thread_mgr)),
        ("输入法开关", openclose_compartment(thread_mgr)),
    ] {
        match compartment.and_then(|compartment| advise_one(&compartment, &sink)) {
            Ok(item) => advice.push(item),
            Err(error) => log(&format!("监听{name}失败: {error}")),
        }
    }
    advice
}

pub(crate) fn unadvise(advice: Advice) {
    for (source, cookie) in advice {
        let _ = unsafe { source.UnadviseSink(cookie) };
    }
}

/// 写系统状态:中文 = 转换模式 NATIVE 位亮 + 输入法开关打开。
/// 已经是目标值时不写,避免触发无意义的 OnChange。
pub(crate) fn write_indicator(thread_mgr: &ITfThreadMgr, tid: u32, chinese: bool) {
    let Ok(manager) = thread_mgr.cast::<ITfCompartmentMgr>() else {
        return;
    };
    if let Ok(compartment) =
        unsafe { manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) }
    {
        let current = read_i32(&compartment);
        let base = current.unwrap_or(NATIVE);
        let next = if chinese {
            base | NATIVE
        } else {
            base & !NATIVE
        };
        if current != Some(next) {
            let _ = unsafe { compartment.SetValue(tid, &VARIANT::from(next)) };
        }
    }
    if let Ok(compartment) = unsafe { manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) }
    {
        let target = i32::from(chinese);
        if read_i32(&compartment) != Some(target) {
            let _ = unsafe { compartment.SetValue(tid, &VARIANT::from(target)) };
        }
    }
}

fn advise_one(
    compartment: &ITfCompartment,
    sink: &ITfCompartmentEventSink,
) -> Result<(ITfSource, u32)> {
    let source: ITfSource = compartment.cast()?;
    let cookie = unsafe { source.AdviseSink(&ITfCompartmentEventSink::IID, sink)? };
    Ok((source, cookie))
}

fn conversion_compartment(thread_mgr: &ITfThreadMgr) -> Result<ITfCompartment> {
    let manager: ITfCompartmentMgr = thread_mgr.cast()?;
    unsafe { manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) }
}

fn openclose_compartment(thread_mgr: &ITfThreadMgr) -> Result<ITfCompartment> {
    let manager: ITfCompartmentMgr = thread_mgr.cast()?;
    unsafe { manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) }
}

/// 转换模式里 NATIVE 位 = 中文;读不到返回 None(不凭猜测改模式)。
fn conversion_chinese(thread_mgr: &ITfThreadMgr) -> Option<bool> {
    let compartment = conversion_compartment(thread_mgr).ok()?;
    read_i32(&compartment).map(|value| value & NATIVE != 0)
}

/// 「输入法开/关」:开 = 中文。系统 Ctrl+Space 翻的就是它。
fn openclose_chinese(thread_mgr: &ITfThreadMgr) -> Option<bool> {
    let compartment = openclose_compartment(thread_mgr).ok()?;
    read_i32(&compartment).map(|value| value != 0)
}

fn read_i32(compartment: &ITfCompartment) -> Option<i32> {
    unsafe { compartment.GetValue() }
        .ok()
        .and_then(|variant| i32::try_from(&variant).ok())
}

#[implement(ITfCompartmentEventSink)]
struct ModeSink {
    thread_mgr: ITfThreadMgr,
    shared: Rc<Shared>,
    client: SharedClient,
}

impl ITfCompartmentEventSink_Impl for ModeSink_Impl {
    fn OnChange(&self, rguid: *const GUID) -> Result<()> {
        if rguid.is_null() {
            return Ok(());
        }
        let guid = unsafe { *rguid };
        // COM 回调不能把 panic 带回 msctf。
        let _ = catch_unwind(AssertUnwindSafe(|| self.sync(guid)));
        Ok(())
    }
}

impl ModeSink_Impl {
    fn sync(&self, guid: GUID) {
        // 自己写 compartment 触发的那次读回与当前一致,直接忽略,避免回环。
        if self.shared.mode_writing.get() {
            return;
        }
        let chinese = if guid == GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION {
            conversion_chinese(&self.thread_mgr)
        } else if guid == GUID_COMPARTMENT_KEYBOARD_OPENCLOSE {
            openclose_chinese(&self.thread_mgr)
        } else {
            None
        };
        let Some(chinese) = chinese else {
            return;
        };
        if chinese == self.shared.chinese.get() {
            return;
        }
        let Some(reply) = request_with(
            &self.client,
            &self.shared,
            Request::SetMode {
                session: self.shared.session,
                chinese,
            },
        ) else {
            return;
        };
        apply_reply(&self.shared, &self.client, &reply);
    }
}

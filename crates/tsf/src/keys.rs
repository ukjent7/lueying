//! 键盘映射:VK → 协议键(按当前键盘布局取字符)、修饰键检测、
//! Shift 单击判定、Caps 同步。

use std::cell::Cell;

use pinyin_protocol::KeyMsg;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, MAPVK_VK_TO_VSC, MapVirtualKeyExW,
    ToUnicodeEx, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_SHIFT,
};

/// 高位为 1(返回值为负)表示按下。
pub(crate) fn shift_down() -> bool {
    unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 }
}

/// 低位为 1 表示 Caps Lock 亮着(锁定键看 toggle 位,不是按住位)。
pub(crate) fn caps_on() -> bool {
    unsafe { GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 }
}

/// 带 Ctrl/Alt 的组合键归应用(快捷键)。
pub(crate) fn command_mods() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 || GetKeyState(VK_MENU.0 as i32) < 0 }
}

pub(crate) fn is_shift(vk: u32) -> bool {
    vk == VK_SHIFT.0 as u32
}

pub(crate) fn is_caps(vk: u32) -> bool {
    vk == VK_CAPITAL.0 as u32
}

/// VK → 协议键;`None` 表示不归输入法。
///
/// 字符层按当前键盘布局翻译(AZERTY/QWERTZ/Dvorak 等),不能把 VK 当 US
/// 布局的字母。`scan` 来自按键消息,`ToUnicodeEx` 需要它区分同一 VK 的
/// 不同物理键。布局查询失败时退回 US 映射,输入法仍可用。
#[allow(clippy::match_same_arms)]
pub(crate) fn vk_to_key(vk: u32, scan: u32, shift: bool) -> Option<KeyMsg> {
    let key = match vk {
        // 小键盘数字同样可以选词;主键区数字走布局翻译,Shift 时是符号。
        0x60..=0x69 => KeyMsg::Digit(vk as u8 - 0x60),
        0x20 => KeyMsg::Space,
        0x0D => KeyMsg::Enter,
        0x1B => KeyMsg::Esc,
        0x08 => KeyMsg::Backspace,
        0x2E => KeyMsg::Delete,
        0x25 => KeyMsg::Left,
        0x26 => KeyMsg::Up,
        0x27 => KeyMsg::Right,
        0x28 => KeyMsg::Down,
        0x24 => KeyMsg::Home,
        0x23 => KeyMsg::End,
        0x21 => KeyMsg::PageUp,
        0x22 => KeyMsg::PageDown,
        _ => match layout_key(vk, scan) {
            Ok(Some(key)) => return Some(key),
            // 布局能翻译但结果不是可输入的 ASCII(死键、非拉丁布局):
            // 直通,让应用自己处理,不能按 US 布局猜一个标点。
            Ok(None) => return None,
            Err(()) => return us_key(vk, shift),
        },
    };
    Some(key)
}

/// 用当前线程的键盘布局翻译字符键。
///
/// `Err` 只在系统调用失败时返回,调用方可以退回 US 映射;死键返回
/// `Ok(None)`,这时必须直通,否则会把别的布局的键位当成 US 标点。
fn layout_key(vk: u32, scan: u32) -> Result<Option<KeyMsg>, ()> {
    let mut state = [0u8; 256];
    unsafe { GetKeyboardState(&mut state) }.map_err(|_| ())?;
    let layout = unsafe { GetKeyboardLayout(0) };
    // SendInput 注入的按键可能没有扫描码;按当前布局补一个,否则字符翻译会失败。
    let scan = if scan == 0 {
        unsafe { MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, Some(layout)) }
    } else {
        scan
    };
    let mut buf = [0u16; 8];
    // bit 2:不修改内核键盘状态。查字符时不能吃掉待组合的死键或连字,
    // 否则应用随后的 TranslateMessage 会得到错误结果。
    const NO_STATE_CHANGE: u32 = 0x4;
    let count = unsafe {
        ToUnicodeEx(
            vk,
            scan,
            &state,
            &mut buf,
            NO_STATE_CHANGE,
            Some(layout),
        )
    };
    // 负数是死键,0 是无可输入字符,多字符是连字;都交给应用。
    if count != 1 {
        return Ok(None);
    }
    let Some(c) = char::from_u32(u32::from(buf[0])) else {
        return Ok(None);
    };
    Ok(match c {
        // Shift+字母在组合中要先把拼音落定再上大写,不能直接直通。
        'a'..='z' | 'A'..='Z' => Some(KeyMsg::Letter(c)),
        '0'..='9' => Some(KeyMsg::Digit(c as u8 - b'0')),
        _ if c.is_ascii_punctuation() => Some(KeyMsg::Punct(c)),
        _ => None,
    })
}

/// 布局翻译失败时的 US 布局兜底。
#[allow(clippy::match_same_arms)]
fn us_key(vk: u32, shift: bool) -> Option<KeyMsg> {
    let key = match vk {
        0x41..=0x5A => {
            let lower = (vk as u8 - b'A' + b'a') as char;
            return Some(KeyMsg::Letter(if shift {
                lower.to_ascii_uppercase()
            } else {
                lower
            }));
        }
        0x30..=0x39 if shift => KeyMsg::Punct(match vk {
            0x31 => '!',
            0x32 => '@',
            0x33 => '#',
            0x34 => '$',
            0x35 => '%',
            0x36 => '^',
            0x37 => '&',
            0x38 => '*',
            0x39 => '(',
            _ => ')',
        }),
        0x30..=0x39 => KeyMsg::Digit(vk as u8 - 0x30),
        0xBC if shift => KeyMsg::Punct('<'),
        0xBC => KeyMsg::Punct(','),
        0xBE if shift => KeyMsg::Punct('>'),
        0xBE => KeyMsg::Punct('.'),
        0xBA if shift => KeyMsg::Punct(':'),
        0xBA => KeyMsg::Punct(';'),
        0xBF if shift => KeyMsg::Punct('?'),
        0xBF => KeyMsg::Punct('/'),
        0xDE if shift => KeyMsg::Punct('"'),
        0xDE => KeyMsg::Punct('\''),
        0xDB if shift => KeyMsg::Punct('{'),
        0xDB => KeyMsg::Punct('['),
        0xDD if shift => KeyMsg::Punct('}'),
        0xDD => KeyMsg::Punct(']'),
        0xDC if shift => KeyMsg::Punct('|'),
        0xDC => KeyMsg::Punct('\\'),
        0xC0 if shift => KeyMsg::Punct('~'),
        0xC0 => KeyMsg::Punct('`'),
        0xBB if shift => KeyMsg::Punct('+'),
        0xBB => KeyMsg::Punct('='),
        0xBD if shift => KeyMsg::Punct('_'),
        0xBD => KeyMsg::Punct('-'),
        _ => return None,
    };
    Some(key)
}

/// Shift 单击判定:按下到抬起之间没有别的键,才算单击切中英。
#[derive(Default)]
pub(crate) struct KeyTap {
    armed: Cell<bool>,
}

impl KeyTap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 每个键的 TestKeyDown 都要报告。
    pub(crate) fn note_down(&self, vk: u32) {
        // Ctrl/Alt+Shift 是系统或应用快捷键,不能算单击切中英。
        self.armed.set(is_shift(vk) && !command_mods());
    }

    /// Shift 抬起时消费判定结果;非 Shift 返回 false。
    pub(crate) fn take_shift_tap(&self, vk: u32) -> bool {
        is_shift(vk) && self.armed.replace(false)
    }
}

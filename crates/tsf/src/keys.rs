//! 键盘映射:VK → 协议键(US 布局符号层)、修饰键检测、Shift 单击判定、Caps 同步。

use std::cell::Cell;

use pinyin_protocol::KeyMsg;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_SHIFT,
};

pub(crate) fn shift_down() -> bool {
    unsafe { GetKeyState(VK_SHIFT).0 as u16 & 0x8000 != 0 }
}

pub(crate) fn caps_on() -> bool {
    unsafe { GetKeyState(VK_CAPITAL).0 as u16 & 0x8000 != 0 }
}

/// 带 Ctrl/Alt 的组合键归应用(快捷键)。
pub(crate) fn command_mods() -> bool {
    unsafe {
        (GetKeyState(VK_CONTROL).0 as u16 & 0x8000 != 0)
            || (GetKeyState(VK_MENU).0 as u16 & 0x8000 != 0)
    }
}

pub(crate) fn is_shift(vk: u32) -> bool {
    vk == VK_SHIFT.0 as u32
}

pub(crate) fn is_caps(vk: u32) -> bool {
    vk == VK_CAPITAL.0 as u32
}

/// VK → 协议键;`None` 表示不归输入法(含 Shift+字母的大写直通)。
#[allow(clippy::match_same_arms)]
pub(crate) fn vk_to_key(vk: u32, shift: bool) -> Option<KeyMsg> {
    let key = match vk {
        0x41..=0x5A => {
            if shift {
                return None;
            }
            return Some(KeyMsg::Letter((vk as u8 - b'A' + b'a') as char));
        }
        0x30..=0x39 => KeyMsg::Digit(vk as u8 - 0x30),
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
        self.armed.set(is_shift(vk));
    }

    /// Shift 抬起时消费判定结果;非 Shift 返回 false。
    pub(crate) fn take_shift_tap(&self, vk: u32) -> bool {
        is_shift(vk) && self.armed.replace(false)
    }
}

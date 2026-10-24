//! 键盘映射:VK → 引擎键(US 布局符号层)、修饰键检测、Shift 单击判定、Caps 同步。

use std::cell::Cell;

use pinyin_engine::Key;
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

/// VK → 引擎键;`None` 表示不归输入法(含 Shift+字母的大写直通)。
#[allow(clippy::match_same_arms)]
pub(crate) fn vk_to_key(vk: u32, shift: bool) -> Option<Key> {
    let key = match vk {
        0x41..=0x5A => {
            if shift {
                return None;
            }
            return Some(Key::Letter((vk as u8 - b'A' + b'a') as char));
        }
        0x30..=0x39 => Key::Digit(vk as u8 - 0x30),
        0x20 => Key::Space,
        0x0D => Key::Enter,
        0x1B => Key::Esc,
        0x08 => Key::Backspace,
        0x2E => Key::Delete,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0xBC if shift => Key::Punct('<'),
        0xBC => Key::Punct(','),
        0xBE if shift => Key::Punct('>'),
        0xBE => Key::Punct('.'),
        0xBA if shift => Key::Punct(':'),
        0xBA => Key::Punct(';'),
        0xBF if shift => Key::Punct('?'),
        0xBF => Key::Punct('/'),
        0xDE if shift => Key::Punct('"'),
        0xDE => Key::Punct('\''),
        0xDB if shift => Key::Punct('{'),
        0xDB => Key::Punct('['),
        0xDD if shift => Key::Punct('}'),
        0xDD => Key::Punct(']'),
        0xDC if shift => Key::Punct('|'),
        0xDC => Key::Punct('\\'),
        0xC0 if shift => Key::Punct('~'),
        0xC0 => Key::Punct('`'),
        0xBB if shift => Key::Punct('+'),
        0xBB => Key::Punct('='),
        0xBD if shift => Key::Punct('_'),
        0xBD => Key::Punct('-'),
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

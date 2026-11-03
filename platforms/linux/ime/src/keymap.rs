//! xkb 键盘事件到引擎按键的映射。
//!
//! 修饰键组合优先交还宿主(Ctrl/Alt/Super),只保留输入法常用的
//! Ctrl+Backspace/Delete/左右方向键做分段编辑。所有判断都不碰 Wayland,
//! 可以在单元测试里直接覆盖。

use lueying_engine::Key;
use xkbcommon::xkb::{self, Keysym, State};

/// 事件到达时的有效修饰状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub caps: bool,
}

impl Modifiers {
    pub fn from_state(state: &State) -> Self {
        Self {
            ctrl: state.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE),
            alt: state.mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE),
            shift: state.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE),
            logo: state.mod_name_is_active(xkb::MOD_NAME_LOGO, xkb::STATE_MODS_EFFECTIVE),
            caps: state.mod_name_is_active(xkb::MOD_NAME_CAPS, xkb::STATE_MODS_EFFECTIVE),
        }
    }
}

/// 映射一个按键;`None` 表示原样交给客户端。
pub fn map(sym: Keysym, text: &str, mods: Modifiers) -> Option<Key> {
    if mods.logo {
        return None;
    }
    if mods.ctrl {
        return match sym {
            Keysym::BackSpace => Some(Key::SegmentBackspace),
            Keysym::Delete => Some(Key::SegmentDelete),
            Keysym::Left => Some(Key::SegmentLeft),
            Keysym::Right => Some(Key::SegmentRight),
            _ => None,
        };
    }
    if mods.alt {
        return None;
    }
    let key = match sym {
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Escape => Key::Esc,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Delete if mods.shift => Key::DeleteCandidate,
        Keysym::Delete => Key::Backspace,
        Keysym::Left => Key::Left,
        Keysym::Right => Key::Right,
        Keysym::Up => Key::Up,
        Keysym::Down => Key::Down,
        Keysym::Home => Key::Home,
        Keysym::End => Key::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        Keysym::Tab => return None,
        _ => {
            let ch = text.chars().next()?;
            match ch {
                ' ' => Key::Space,
                c if c.is_ascii_alphabetic() => Key::Letter(c.to_ascii_lowercase()),
                c if c.is_ascii_digit() => Key::Digit(c.to_digit(10)? as u8),
                c if c.is_ascii() && !c.is_control() => Key::Punct(c),
                _ => return None,
            }
        }
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn navigation_and_editing_keys_map_directly() {
        assert_eq!(map(Keysym::Return, "\r", plain()), Some(Key::Enter));
        assert_eq!(map(Keysym::Escape, "", plain()), Some(Key::Esc));
        assert_eq!(map(Keysym::Delete, "", plain()), Some(Key::Backspace));
        assert_eq!(map(Keysym::BackSpace, "", plain()), Some(Key::Backspace));
        assert_eq!(map(Keysym::Page_Up, "", plain()), Some(Key::PageUp));
        assert_eq!(map(Keysym::Page_Down, "", plain()), Some(Key::PageDown));
    }

    #[test]
    fn shift_delete_removes_a_learned_candidate() {
        let mods = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(map(Keysym::Delete, "", mods), Some(Key::DeleteCandidate));
    }

    #[test]
    fn control_shortcuts_keep_only_segment_editing() {
        let mods = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            map(Keysym::BackSpace, "", mods),
            Some(Key::SegmentBackspace)
        );
        assert_eq!(map(Keysym::Delete, "", mods), Some(Key::SegmentDelete));
        assert_eq!(map(Keysym::Left, "", mods), Some(Key::SegmentLeft));
        assert_eq!(map(Keysym::Right, "", mods), Some(Key::SegmentRight));
        assert_eq!(map(Keysym::a, "a", mods), None);
    }

    #[test]
    fn printable_text_becomes_letters_digits_and_punctuation() {
        assert_eq!(map(Keysym::a, "a", plain()), Some(Key::Letter('a')));
        assert_eq!(map(Keysym::A, "A", plain()), Some(Key::Letter('a')));
        assert_eq!(map(Keysym::_1, "1", plain()), Some(Key::Digit(1)));
        assert_eq!(map(Keysym::comma, ",", plain()), Some(Key::Punct(',')));
        assert_eq!(map(Keysym::space, " ", plain()), Some(Key::Space));
    }

    #[test]
    fn alt_and_super_pass_through() {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let logo = Modifiers {
            logo: true,
            ..Modifiers::default()
        };
        assert_eq!(map(Keysym::a, "a", alt), None);
        assert_eq!(map(Keysym::a, "a", logo), None);
    }
}

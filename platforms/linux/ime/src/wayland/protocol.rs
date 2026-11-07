//! 手工生成的 Wayland 协议绑定。
//!
//! `wayland-protocols` 0.32 只带 input-method-unstable-v1;v2 与虚拟键盘
//! 的 XML 随仓库分发,由 `wayland-scanner` 在编译期生成客户端代码。

pub mod input_method {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/input-method-unstable-v2.xml");
    }

    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/input-method-unstable-v2.xml");

    /// `input-method-unstable-v2.xml` 的 `content_type` / `text_change_cause`
    /// 事件借用了 `zwp_text_input_v3` 的枚举。本外壳不生成 v3 接口,
    /// 只补上生成代码引用的三个类型。
    pub mod zwp_text_input_v3 {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u32)]
        pub enum ChangeCause {
            InputMethod = 0,
            Other = 1,
        }

        impl TryFrom<u32> for ChangeCause {
            type Error = u32;

            fn try_from(value: u32) -> Result<Self, Self::Error> {
                match value {
                    0 => Ok(Self::InputMethod),
                    1 => Ok(Self::Other),
                    other => Err(other),
                }
            }
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u32)]
        pub enum ContentHint {
            None = 0,
            Completion = 1,
            Spellcheck = 2,
            AutoCapitalization = 4,
            Lowercase = 8,
            Uppercase = 16,
            Titlecase = 32,
            Hidden = 64,
            SensitiveData = 128,
            Latin = 256,
            Multiline = 512,
        }

        impl TryFrom<u32> for ContentHint {
            type Error = u32;

            fn try_from(value: u32) -> Result<Self, Self::Error> {
                match value {
                    0 => Ok(Self::None),
                    1 => Ok(Self::Completion),
                    2 => Ok(Self::Spellcheck),
                    4 => Ok(Self::AutoCapitalization),
                    8 => Ok(Self::Lowercase),
                    16 => Ok(Self::Uppercase),
                    32 => Ok(Self::Titlecase),
                    64 => Ok(Self::Hidden),
                    128 => Ok(Self::SensitiveData),
                    256 => Ok(Self::Latin),
                    512 => Ok(Self::Multiline),
                    other => Err(other),
                }
            }
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u32)]
        pub enum ContentPurpose {
            Normal = 0,
            Alpha = 1,
            Digits = 2,
            Number = 3,
            Phone = 4,
            Url = 5,
            Email = 6,
            Name = 7,
            Password = 8,
            Date = 9,
            Time = 10,
            Datetime = 11,
            Terminal = 12,
        }

        impl TryFrom<u32> for ContentPurpose {
            type Error = u32;

            fn try_from(value: u32) -> Result<Self, Self::Error> {
                match value {
                    0 => Ok(Self::Normal),
                    1 => Ok(Self::Alpha),
                    2 => Ok(Self::Digits),
                    3 => Ok(Self::Number),
                    4 => Ok(Self::Phone),
                    5 => Ok(Self::Url),
                    6 => Ok(Self::Email),
                    7 => Ok(Self::Name),
                    8 => Ok(Self::Password),
                    9 => Ok(Self::Date),
                    10 => Ok(Self::Time),
                    11 => Ok(Self::Datetime),
                    12 => Ok(Self::Terminal),
                    other => Err(other),
                }
            }
        }
    }
}

pub mod virtual_keyboard {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/virtual-keyboard-unstable-v1.xml");
    }

    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/virtual-keyboard-unstable-v1.xml");
}

/// 工具栏用的 layer-shell 客户端子集,XML 见 `protocols/wlr-layer-shell-unstable-v1.xml`。
pub mod layer_shell {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/wlr-layer-shell-unstable-v1.xml");
    }

    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/wlr-layer-shell-unstable-v1.xml");
}

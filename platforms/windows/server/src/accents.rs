//! 候选窗主题色预设。
//!
//! 预设表本身不依赖 Windows:设置窗列出可选主题色,候选窗按同一张表上色,
//! 两边不会走样。

/// 主题色预设。`light`/`dark` 各是 [accent, accent_hover, accent_text,
/// selected_bg];高对比度主题始终用系统色,不吃预设。
pub struct Accent {
    pub name: &'static str,
    pub light: [u32; 4],
    pub dark: [u32; 4],
}

/// 首位是出厂蓝;新增预设加在表尾,旧配置里的下标不变。
pub const ACCENTS: [Accent; 6] = [
    Accent {
        name: "蓝",
        light: [0x2f6fed, 0x255fd0, 0x2459c9, 0xeaf1ff],
        dark: [0x2f6fed, 0x255fd0, 0x7aa7ff, 0x2a3b57],
    },
    Accent {
        name: "青",
        light: [0x0d7377, 0x0a5f63, 0x095457, 0xe3f2f2],
        dark: [0x1fb6bd, 0x1aa0a6, 0x6fd8dd, 0x1c3f42],
    },
    Accent {
        name: "绿",
        light: [0x167a34, 0x116329, 0x0e5526, 0xe5f3ea],
        dark: [0x2fae5e, 0x279750, 0x83d3a1, 0x1c3a27],
    },
    Accent {
        name: "紫",
        light: [0x6b4fd8, 0x5a41c4, 0x4f38b0, 0xefecfc],
        dark: [0x8f78ee, 0x7d65e2, 0xbcaafb, 0x2c2547],
    },
    Accent {
        name: "红",
        light: [0xb4342c, 0x9c2a24, 0x8c251f, 0xf9e9e8],
        dark: [0xd75f57, 0xc74e46, 0xf0a09a, 0x452320],
    },
    Accent {
        name: "橙",
        light: [0xb25e12, 0x994f0d, 0x8a480c, 0xf9efe2],
        dark: [0xd9862e, 0xc37522, 0xf0b266, 0x44301b],
    },
];

/// 主题色配置值转下标;接受颜色名或下标,越界回出厂蓝。
pub fn accent_index(value: &str) -> Option<usize> {
    if let Ok(index) = value.parse::<usize>() {
        return (index < ACCENTS.len()).then_some(index);
    }
    ACCENTS.iter().position(|a| a.name == value)
}

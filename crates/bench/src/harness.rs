//! E2E 公共件:检查记录与报告表格、临时目录与渲染夹具。

use std::path::PathBuf;

use lueying_render::panel::{Item, Layout, Palette, PanelSpec, PreeditBar};

/// 一条区域检查表:`(检查名, 是否通过, 说明)`。
pub type Checks = Vec<(&'static str, bool, String)>;

/// 记一条检查结果。
pub fn push(checks: &mut Checks, name: &'static str, ok: bool, note: String) {
    checks.push((name, ok, note));
}

/// 把一张检查表写成 Markdown 表格;`|` 与换行转义后进报告,返回失败数。
pub fn write(report: &mut String, title: &str, checks: Checks) -> usize {
    report.push_str(&format!(
        "\n## {title}\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n"
    ));
    let mut failures = 0usize;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        let note = note.replace('|', "\\|").replace('\n', " ");
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}

/// 临时目录;测试结束整棵删掉。
pub struct Temp {
    pub root: PathBuf,
}

impl Temp {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root =
            std::env::temp_dir().join(format!("lueying-e2e-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).expect("建临时目录");
        Self { root }
    }

    /// 在临时目录下建一个子目录并返回。
    pub fn pack(&self, dir: &str) -> PathBuf {
        let path = self.root.join(dir);
        std::fs::create_dir_all(&path).expect("建扩展目录");
        path
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// E2E 样例面板规格;渲染族三个检查文件共用同一份期望值。
pub fn spec() -> PanelSpec {
    PanelSpec {
        width: 360.0,
        max_text_width: 300.0,
        preedit: PreeditBar {
            text: "nihao".into(),
            page_label: "1/2".into(),
        },
        items: vec![
            Item {
                text: "你好".into(),
                hint: String::new(),
                selected: true,
            },
            Item {
                text: "你号".into(),
                hint: String::new(),
                selected: false,
            },
        ],
        layout: Layout::Wrap,
    }
}

/// E2E 样例调色板。
pub fn palette() -> Palette {
    Palette {
        bg: 0xffffff,
        window_bg: 0xf3f4f7,
        surface_alt: 0xf2f2f2,
        border: 0xdddddd,
        border_strong: 0xcfd2d9,
        text: 0x111111,
        text_secondary: 0x444444,
        text_tertiary: 0x6a6e77,
        accent: 0x2f6fed,
        on_accent: 0xffffff,
        segment: 0xffffff,
        off_bg: 0xd9dce3,
        knob_off: 0xffffff,
        danger: 0xb3261e,
        danger_soft: 0xfdecea,
        accent_text: 0x2459c9,
        selected_bg: 0xeaf1ff,
        selected_text: 0x11224d,
        hover: 0xf0f2f6,
    }
}

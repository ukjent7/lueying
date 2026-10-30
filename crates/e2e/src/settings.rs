//! 设置页模型 E2E:页面结构、点击改配置、键盘改值与焦点顺序。
//!
//! 先列失败方式:
//! 1. 页面少一行或分组顺序不对,设置窗里就找不到那个开关。
//! 2. 点了开关配置没变,用户以为改了其实没保存。
//! 3. 分段组点哪格都不换值,或者左右键绕回自己。
//! 4. Tab 走到一半断了,键盘用户够不到后面的控件。
//! 5. 控件排到卡片外面,点不到也看不见。

use lueying_extmeta::metrics::{ExtMetrics, ExtStat, ExtState};
use lueying_features::SchemeId;
use lueying_render::ui::{self, Id};
use lueying_server::config::{Channel, Region, UiFont, UiLayout};
use lueying_server::settings_view::{Action, PageColors, ScriptEntry, Settings, ids};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

/// 页面用的颜色:E2E 只关心结构,颜色给固定值。
fn colors() -> PageColors {
    PageColors {
        notice_info: 0x5f636b,
        notice_success: 0x167a34,
        notice_error: 0xb3261e,
        accents: vec![0x2f6fed; 6],
    }
}

/// 设置窗的样例状态。
fn settings() -> Settings {
    Settings {
        current: "0.1.0".to_string(),
        ..Settings::default()
    }
}

/// 找一行的说明文字;行不在就是 None。
fn row_detail(page: &ui::Page, group: &str, label: &str) -> Option<String> {
    page.groups
        .iter()
        .find(|g| g.title == group)
        .and_then(|g| g.rows.iter().find(|r| r.label == label))
        .map(|r| r.detail.clone())
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();
    let mut settings = settings();
    settings.scripts = vec![
        ScriptEntry::new("demo-pack", "演示包"),
        ScriptEntry::new("other-pack", "另一个包"),
    ];
    let page = settings.page(&colors());

    // 五个分组,顺序固定。
    let titles: Vec<&str> = page.groups.iter().map(|g| g.title.as_str()).collect();
    push(
        &mut checks,
        "设置分组齐全",
        titles == ["输入", "候选", "扩展", "词库", "关于"],
        format!("{titles:?}"),
    );
    // 每组都有行,每行都有标题。
    let empty_rows = page
        .groups
        .iter()
        .flat_map(|g| g.rows.iter())
        .filter(|r| r.label.is_empty())
        .count();
    push(
        &mut checks,
        "设置行都有标题",
        empty_rows == 0 && page.groups.iter().all(|g| !g.rows.is_empty()),
        format!("空标题 {empty_rows} 行"),
    );
    // 关键开关都在。
    let aux = row_detail(&page, "输入", "辅助码");
    let punct = row_detail(&page, "输入", "中文标点");
    push(
        &mut checks,
        "设置关键行在位",
        aux.is_some() && punct.is_some(),
        format!("辅助码={aux:?} 中文标点={punct:?}"),
    );

    // 运行期脚本行:未启用时给提示,启用后显示名字与 id;「选择」开菜单,
    // 「目录」交给平台侧;模型本身负责切换与关闭。
    let idle = row_detail(&page, "扩展", "运行期脚本");
    settings.cfg.extensions = vec!["demo-pack".to_string()];
    let active = row_detail(&settings.page(&colors()), "扩展", "运行期脚本");
    let choose = settings.activate(&Id::new(ids::SCRIPTS, 0));
    let open = settings.activate(&Id::new(ids::SCRIPTS_DIR, 0));
    let consent = settings.activate(&Id::new(ids::CONSENT, 0));
    let audit = settings.activate(&Id::new(ids::AUDIT, 0));
    let metrics = settings.activate(&Id::new(ids::EXT_METRICS, 0));
    let consent_row = row_detail(&settings.page(&colors()), "扩展", "扩展授权");
    let audit_row = row_detail(&settings.page(&colors()), "扩展", "审计日志");
    let stat = ExtStat {
        id: "demo-pack".to_string(),
        name: "演示包".to_string(),
        state: ExtState::Loaded,
        step_peak_us: 1500,
        ..ExtStat::default()
    };
    settings.ext_metrics = Some(ExtMetrics {
        extensions: vec![stat],
        ..ExtMetrics::default()
    });
    let metrics_row = row_detail(&settings.page(&colors()), "扩展", "扩展运行");
    settings.select_script(Some(1));
    let switched = settings.cfg.extensions == vec!["other-pack"];
    settings.select_script(None);
    settings.cfg.extensions = vec!["gone".to_string()];
    let missing = row_detail(&settings.page(&colors()), "扩展", "运行期脚本");
    settings.select_script(None);
    push(
        &mut checks,
        "设置运行期脚本选择",
        idle.as_deref()
            .is_some_and(|text| text.contains("未启用") && text.contains('2'))
            && active
                .as_deref()
                .is_some_and(|text| text.contains("演示包") && text.contains("demo-pack"))
            && choose == Some(Action::ChooseScript)
            && open == Some(Action::OpenScripts)
            && consent == Some(Action::ApproveExtension)
            && audit == Some(Action::OpenExtAudit)
            && metrics == Some(Action::RefreshExtMetrics)
            && consent_row
                .as_deref()
                .is_some_and(|text| text.contains("无需授权"))
            && audit_row
                .as_deref()
                .is_some_and(|text| text.contains("截图"))
            && metrics_row.as_deref().is_some_and(|text| {
                text.contains("运行 1") && text.contains("步进峰值 1.5 ms")
            })
            && switched
            && settings.cfg.extensions.is_empty()
            && missing
                .as_deref()
                .is_some_and(|text| text.contains("未安装")),
        format!(
            "未启用 {idle:?} 启用后 {active:?} 未安装 {missing:?} 动作 {choose:?}/{open:?}/{consent:?}/{audit:?}/{metrics:?} 切换 {switched}"
        ),
    );

    // 点开关:配置跟着翻。
    let before = settings.cfg.aux;
    settings.activate(&Id::new(ids::AUX, 0));
    push(
        &mut checks,
        "设置开关改配置",
        settings.cfg.aux != before,
        format!("辅助码 {before} → {}", settings.cfg.aux),
    );
    // 点分段格:值跟着换。
    settings.activate(&Id::new(ids::SCHEME, 2));
    push(
        &mut checks,
        "设置分段改配置",
        settings.cfg.scheme == SchemeId::Ziranma,
        format!("方案={:?}", settings.cfg.scheme),
    );
    // 别的分段组也认自己的下标。
    settings.activate(&Id::new(ids::REGION, 2));
    settings.activate(&Id::new(ids::LAYOUT, 1));
    settings.activate(&Id::new(ids::FONT, 0));
    settings.activate(&Id::new(ids::CHANNEL, 1));
    push(
        &mut checks,
        "设置各分段组独立",
        settings.cfg.region == Region::HongKong
            && settings.cfg.ui_layout == UiLayout::Vertical
            && settings.cfg.ui_font == UiFont::Small
            && settings.cfg.update_channel == Channel::Beta,
        format!(
            "地区={:?} 排列={:?} 字号={:?} 渠道={:?}",
            settings.cfg.region,
            settings.cfg.ui_layout,
            settings.cfg.ui_font,
            settings.cfg.update_channel
        ),
    );
    // 左右键循环:往后一步,再往前一步回到原处;绕回也停得住。
    let before = settings.cfg.scheme;
    settings.shift(&Id::new(ids::SCHEME, 0), 1);
    let stepped = settings.cfg.scheme;
    settings.shift(&Id::new(ids::SCHEME, 0), -1);
    push(
        &mut checks,
        "设置左右键循环",
        stepped != before && settings.cfg.scheme == before,
        format!("{before:?} → {stepped:?} → {:?}", settings.cfg.scheme),
    );
    // 绕回:从第一格再往前一步,回到最后一格。
    settings.activate(&Id::new(ids::SCHEME, 0));
    settings.shift(&Id::new(ids::SCHEME, 0), -1);
    push(
        &mut checks,
        "设置左右键绕回",
        settings.cfg.scheme == SchemeId::Sogou,
        format!("{:?}", settings.cfg.scheme),
    );
    // 非分段组不吃左右键。
    let aux = settings.cfg.aux;
    let changed = settings.shift(&Id::new(ids::AUX, 0), 1);
    push(
        &mut checks,
        "设置开关不吃左右键",
        !changed && settings.cfg.aux == aux,
        format!("changed={changed} 辅助码={}", settings.cfg.aux),
    );

    // 焦点顺序:每个控件只出现一次,头尾是更新与保存。
    let order = Settings::focus_order();
    let mut sorted = order.clone();
    sorted.sort_by_key(|id| (id.base, id.index));
    let mut unique = sorted.clone();
    unique.dedup();
    push(
        &mut checks,
        "设置焦点顺序不重",
        unique.len() == order.len() && order.len() > 20,
        format!("{} 项,去重后 {}", order.len(), unique.len()),
    );
    push(
        &mut checks,
        "设置焦点首尾正确",
        order.first() == Some(&Id::new(ids::UPDATE, 0))
            && order.last() == Some(&Id::new(ids::SAVE, 0)),
        format!("{:?}…{:?}", order.first(), order.last()),
    );
    // 焦点顺序里的控件都得在页面上找得到。
    let missing: Vec<&str> = order
        .iter()
        .filter(|id| !Settings::is_segment(id.base) && !page.has_control(**id))
        .map(|id| id.base)
        .collect();
    push(
        &mut checks,
        "设置焦点控件都在页面上",
        missing.is_empty(),
        format!("缺 {missing:?}"),
    );

    // 排版:控件都在卡片里,页面能排出来。
    let font = ui_font();
    let layout = ui::layout(&page, &font, 1.0, 650.0);
    let outside: Vec<&str> = order
        .iter()
        .filter_map(|id| layout.card_hit(*id))
        .filter(|rect| rect.left < 0.0 || rect.right > layout.card.width())
        .map(|_| "越界")
        .collect();
    push(
        &mut checks,
        "设置控件不出卡片",
        outside.is_empty(),
        format!("越界 {} 个", outside.len()),
    );
    // 页脚按钮在视口底上。
    push(
        &mut checks,
        "设置页脚贴视口底",
        (layout.footer.bottom - 632.0).abs() < 0.5,
        format!("页脚下沿={:.1}", layout.footer.bottom),
    );

    // 配置往返:extensions 键要能读、能原样存回;路径分隔符不能当扩展 id。
    let path = std::env::temp_dir().join(format!("lueying-config-e2e-{}", std::process::id()));
    let _ = std::fs::write(&path, "extensions=my-pack,other-pack\n");
    let cfg = lueying_server::config::load(&path);
    let loaded = cfg.extensions == vec!["my-pack", "other-pack"];
    let saved =
        lueying_server::config::save(&path, &cfg).and_then(|()| std::fs::read_to_string(&path));
    let _ = std::fs::remove_file(&path);
    push(
        &mut checks,
        "配置保留扩展列表与顺序",
        loaded
            && saved
                .as_deref()
                .is_ok_and(|text| text.contains("extensions=my-pack,other-pack")),
        format!("读取 {loaded} 存回 {:?}", saved.as_deref().map(str::len)),
    );
    let path = std::env::temp_dir().join(format!("lueying-config-e2e-bad-{}", std::process::id()));
    let _ = std::fs::write(&path, "extensions=../evil,ok-pack\n");
    let cfg = lueying_server::config::load(&path);
    let rejected = cfg.extensions == vec!["ok-pack"];
    let _ = std::fs::remove_file(&path);
    push(
        &mut checks,
        "扩展 id 拒绝路径",
        rejected,
        "config.ini 的 extensions=../evil 应被忽略".into(),
    );

    report.push_str("\n## 设置页模型\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}

/// 设置窗用的字型;系统没有字型时退回默认字型(只影响宽度测量)。
fn ui_font() -> skia_safe::Font {
    let mgr = skia_safe::FontMgr::new();
    match mgr.legacy_make_typeface(None, skia_safe::FontStyle::normal()) {
        Some(face) => skia_safe::Font::from_typeface(face, 17.0),
        None => skia_safe::Font::default(),
    }
}

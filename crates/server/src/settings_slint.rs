//! 设置窗的 Slint 验证:`lueying-settings --slint` 起一页 winit + 软件
//! 渲染的 Fluent 窗口,量迁移成本、验证配置绑定与系统深浅色;这也是
//! 全量迁移的脚手架:平台面就这些——组件、属性、回调、事件循环。

use lueying_net::update;

use lueying_server::config::{self, AppConfig, Channel};
use lueying_server::settings_actions;
use lueying_server::settings_view::{Notice, Settings};

slint::include_modules!();

/// 起窗口并跑事件循环;窗口关闭才返回。
pub fn run_spike() {
    let path = config::path();
    let cfg = path
        .as_ref()
        .map_or_else(AppConfig::default, |path| config::load(path));
    let ui = SettingsSpike::new().expect("Slint 设置组件创建失败");
    ui.set_version(update::current_version());
    ui.set_channel(channel_text(cfg.update_channel));
    ui.set_effects(cfg.effects);
    let handle = ui.as_weak();
    let save_path = path.clone();
    ui.on_toggle_effects(move |checked| {
        let Some(ui) = handle.upgrade() else {
            return;
        };
        let mut settings = load(&save_path);
        settings.cfg.effects = checked;
        settings_actions::save(&mut settings);
        ui.set_notice(notice_text(&settings));
    });
    let handle = ui.as_weak();
    ui.on_open_scene(move || {
        let Some(ui) = handle.upgrade() else {
            return;
        };
        let mut settings = load(&path);
        settings_actions::open_scene(&mut settings);
        ui.set_notice(notice_text(&settings));
    });
    ui.run().expect("Slint 事件循环退出失败");
}

/// 从磁盘读一份配置当工作台;每次改动都以落盘配置为基线。
fn load(path: &Option<std::path::PathBuf>) -> Settings {
    let mut settings = path.as_ref().map_or_else(
        || Settings::with_config(AppConfig::default()),
        |path| Settings::with_config(config::load(path)),
    );
    settings.path = path.clone();
    settings
}

/// 动作留下的提示回填到界面;没有提示就清空。
fn notice_text(settings: &Settings) -> String {
    match settings.notice {
        Notice::None => String::new(),
        _ => settings.message.clone(),
    }
}

fn channel_text(channel: Channel) -> String {
    match channel {
        Channel::Stable => "稳定".to_string(),
        Channel::Beta => "测试".to_string(),
    }
}

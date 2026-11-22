//! 声明式扩展设置:清单 schema 进设置页,值落在 `config.toml` 的 `[ext.<id>]`。
//!
//! 这一层不碰 Windows:字段展开、当前值与点击后的新值都在这里,设置窗
//! 只负责画。扩展没安装时读不到 schema,配置里已有的值原样保留。

use lueying_ext::manifest::{Setting, SettingValue};
use serde_json::Value;

use crate::settings::consent::ScriptEntry;
use crate::settings::model::{Button, ButtonKind, Control, Group, Id, Row, SegmentOption};
use crate::settings::view::ids;
use lueying_server::config::AppConfig;

/// 一张已安装卡片的清单字段。位置或启用顺序都不是字段的身份。
pub(super) struct Field<'a> {
    pub(super) script: &'a ScriptEntry,
    pub(super) setting: &'a Setting,
}

impl<'a> Field<'a> {
    pub(super) fn find(scripts: &'a [ScriptEntry], base: &str) -> Option<Self> {
        let (id, key) = ids::ext_field_parts(base)?;
        let script = scripts.iter().find(|script| script.id == id)?;
        let setting = script
            .settings
            .iter()
            .find(|setting| setting.key() == key)?;
        Some(Self { script, setting })
    }

    fn value(&self, cfg: &AppConfig) -> SettingValue {
        let stored = cfg
            .ext_settings
            .get(&self.script.id)
            .and_then(|table| table.get(self.setting.key()));
        self.setting.resolve(stored)
    }

    fn on(&self, cfg: &AppConfig) -> bool {
        self.value(cfg) == SettingValue::Bool(true)
    }

    pub(super) fn selected(&self, cfg: &AppConfig) -> usize {
        let options = self.setting.options();
        if options.is_empty() {
            return 0;
        }
        match self.value(cfg) {
            SettingValue::Text(text) => options
                .iter()
                .position(|option| option == &text)
                .unwrap_or(0),
            SettingValue::Bool(_) => 0,
        }
    }

    fn row(&self, cfg: &AppConfig) -> Row {
        let setting = self.setting;
        let id = Id::new(ids::ext_field(&self.script.id, setting.key()), 0);
        let mut detail = if setting.detail.trim().is_empty() {
            format!("{} 声明的设置", self.script.name)
        } else {
            setting.detail.clone()
        };
        let control = if setting.is_toggle() {
            Control::Toggle {
                id,
                on: self.on(cfg),
            }
        } else if setting.is_secret() {
            let filled = crate::ext_secret::has(&self.script.id, setting.key());
            detail.push_str(if filled {
                " · 已设置(仅本机保存)"
            } else {
                " · 未设置(仅本机保存)"
            });
            Control::Buttons {
                items: vec![Button {
                    id,
                    text: if filled { "更换" } else { "设置" }.to_string(),
                    kind: if filled {
                        ButtonKind::Secondary
                    } else {
                        ButtonKind::Primary
                    },
                    enabled: true,
                }],
            }
        } else {
            Control::Segment {
                id,
                options: setting
                    .options()
                    .iter()
                    .cloned()
                    .map(SegmentOption::new)
                    .collect(),
                selected: self.selected(cfg),
                cell_width: Some(44.0),
            }
        };
        Row {
            label: setting.title.clone(),
            detail,
            control,
        }
    }

    /// 只写本地普通设置;密钥走独立的编辑动作。
    pub(super) fn apply(&self, cfg: &mut AppConfig, index: usize) -> bool {
        let value = if self.setting.is_toggle() {
            Value::Bool(!self.on(cfg))
        } else if self.setting.is_secret() {
            return false;
        } else {
            let Some(option) = self.setting.options().get(index) else {
                return false;
            };
            Value::String(option.clone())
        };
        cfg.ext_settings
            .entry(self.script.id.clone())
            .or_default()
            .insert(self.setting.key.clone(), value);
        true
    }
}

pub(super) fn group(script: &ScriptEntry, cfg: &AppConfig) -> Group {
    Group {
        title: format!("扩展 · {}", script.name),
        rows: script
            .settings
            .iter()
            .map(|setting| Field { script, setting }.row(cfg))
            .collect(),
    }
}

/// 删掉清单不认或类型不符的设置值;返回删除项数。
pub fn normalize(cfg: &mut AppConfig, scripts: &[ScriptEntry]) -> usize {
    let mut removed = 0;
    for script in scripts {
        let Some(table) = cfg.ext_settings.get_mut(&script.id) else {
            continue;
        };
        for setting in &script.settings {
            // 密钥只进私密存储;配置里旧版本留下的值在保存时清掉。
            if setting.is_secret() {
                if table.remove(setting.key()).is_some() {
                    removed += 1;
                }
                continue;
            }
            let bad = table
                .get(setting.key())
                .is_some_and(|value| !setting.accepts(value));
            if bad {
                table.remove(setting.key());
                removed += 1;
            }
        }
        let known: Vec<&str> = script.settings.iter().map(Setting::key).collect();
        let before = table.len();
        table.retain(|key, _| known.contains(&key.as_str()));
        removed += before - table.len();
    }
    cfg.ext_settings.retain(|_, table| !table.is_empty());
    removed
}

#[cfg(test)]
mod tests;

//! 声明式扩展设置:清单 schema 进设置页,值落在 `config.toml` 的 `[ext.<id>]`。
//!
//! 这一层不碰 Windows:字段展开、当前值与点击后的新值都在这里,设置窗
//! 只负责画。扩展没安装时读不到 schema,配置里已有的值原样保留。

use std::collections::BTreeMap;

use lueying_extmeta::{Manifest, Setting, SettingValue};
use lueying_render::ui::{Button, ButtonKind, Control, Group, Id, Row, SegmentOption};
use serde_json::Value;

use crate::config::AppConfig;
use crate::settings_consent::ScriptEntry;
use crate::settings_view::ids;

/// 一页最多渲染的字段数;超出的值仍可在 `config.toml` 里手改。
pub const MAX_FIELDS: usize = ids::EXT_FIELDS;

/// 设置页上的一个字段:清单声明 + 按当前配置解析好的值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// 静态控件前缀下标;见 [`ids::EXT_FIELDS`]。
    pub slot: usize,
    /// 扩展 id。
    pub ext: String,
    /// 扩展显示名。
    pub name: String,
    /// `[ext.<id>]` 下的键。
    pub key: String,
    pub title: String,
    pub detail: String,
    pub kind: Kind,
}

/// 宿主支持的控件;类型在清单加载期已经限死。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Toggle {
        on: bool,
    },
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    /// 密钥文本:设置页显示是否已设置,编辑走原生单行输入窗。
    Secret {
        filled: bool,
    },
}

impl Field {
    /// 分段组当前选中的格;开关与密钥固定 0。
    pub fn selected(&self) -> usize {
        match &self.kind {
            Kind::Toggle { .. } | Kind::Secret { .. } => 0,
            Kind::Choice { selected, .. } => *selected,
        }
    }

    /// 是否密钥字段;这类字段不进配置文件。
    pub fn is_secret(&self) -> bool {
        matches!(&self.kind, Kind::Secret { .. })
    }
}

/// 启用的扩展 → 字段,按启用顺序、清单声明顺序。
pub fn fields(extensions: &[String], scripts: &[ScriptEntry], cfg: &AppConfig) -> Vec<Field> {
    let mut out = Vec::new();
    'exts: for id in extensions {
        let Some(script) = scripts.iter().find(|script| &script.id == id) else {
            continue;
        };
        for setting in &script.settings {
            if out.len() >= MAX_FIELDS {
                break 'exts;
            }
            out.push(field(out.len(), script, setting, cfg));
        }
    }
    out
}

fn field(slot: usize, script: &ScriptEntry, setting: &Setting, cfg: &AppConfig) -> Field {
    let stored = cfg
        .ext_settings
        .get(&script.id)
        .and_then(|table| table.get(setting.key()));
    let value = setting.resolve(stored);
    let kind = if setting.is_toggle() {
        Kind::Toggle {
            on: matches!(&value, SettingValue::Bool(true)),
        }
    } else if setting.is_secret() {
        Kind::Secret {
            filled: crate::settings_secret::has(&script.id, setting.key()),
        }
    } else {
        let selected = match &value {
            SettingValue::Text(text) => setting
                .options()
                .iter()
                .position(|option| option == text)
                .unwrap_or(0),
            SettingValue::Bool(_) => 0,
        };
        Kind::Choice {
            options: setting.options().to_vec(),
            selected,
        }
    };
    let mut detail = if setting.detail.trim().is_empty() {
        format!("{} 声明的设置", script.name)
    } else {
        setting.detail.clone()
    };
    if let Kind::Secret { filled } = &kind {
        detail.push_str(if *filled {
            " · 已设置(仅本机保存)"
        } else {
            " · 未设置(仅本机保存)"
        });
    }
    Field {
        slot,
        ext: script.id.clone(),
        name: script.name.clone(),
        key: setting.key().to_string(),
        title: setting.title.clone(),
        detail,
        kind,
    }
}

/// 扩展设置分组:每个扩展一组,组内一行一个字段。
pub fn groups(extensions: &[String], scripts: &[ScriptEntry], cfg: &AppConfig) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut last: Option<String> = None;
    for field in fields(extensions, scripts, cfg) {
        let control = control(&field);
        let row = Row {
            label: field.title,
            detail: field.detail,
            control,
        };
        if last.as_deref() == Some(field.ext.as_str()) {
            if let Some(group) = groups.last_mut() {
                group.rows.push(row);
            }
        } else {
            last = Some(field.ext);
            groups.push(Group {
                title: format!("扩展 · {}", field.name),
                rows: vec![row],
            });
        }
    }
    groups
}

fn control(field: &Field) -> Control {
    let id = Id::new(ids::ext_field(field.slot), 0);
    match &field.kind {
        Kind::Toggle { on } => Control::Toggle { id, on: *on },
        Kind::Choice { options, selected } => Control::Segment {
            id,
            options: options
                .iter()
                .map(|option| SegmentOption::new(option.clone()))
                .collect(),
            selected: *selected,
            width: 44.0,
        },
        Kind::Secret { filled } => Control::Buttons {
            items: vec![Button {
                id,
                text: if *filled { "更换" } else { "设置" }.to_string(),
                kind: if *filled {
                    ButtonKind::Secondary
                } else {
                    ButtonKind::Primary
                },
                enabled: true,
            }],
        },
    }
}

/// 按点击的格改配置:开关翻转,单选选中该格。
pub fn apply(cfg: &mut AppConfig, field: &Field, index: usize) -> bool {
    let table = cfg.ext_settings.entry(field.ext.clone()).or_default();
    match &field.kind {
        Kind::Toggle { on } => {
            table.insert(field.key.clone(), Value::Bool(!*on));
            true
        }
        Kind::Choice { options, .. } => {
            let Some(option) = options.get(index) else {
                return false;
            };
            table.insert(field.key.clone(), Value::String(option.clone()));
            true
        }
        Kind::Secret { .. } => false,
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

/// 扩展设置的完整值:配置里的值,缺省补清单默认值;密钥从私密存储并进。
pub fn values(ext: &str, settings: &[Setting], stored: Option<&BTreeMap<String, Value>>) -> Value {
    let mut object = serde_json::Map::new();
    for setting in settings {
        let value = setting.resolve(stored.and_then(|table| table.get(setting.key())));
        object.insert(setting.key().to_string(), value.to_json());
    }
    crate::settings_secret::merge(ext, settings, &mut object);
    Value::Object(object)
}

/// 读已安装扩展的声明式设置;包不在、id 不合法或清单坏时 None。
pub fn manifest_settings(id: &str) -> Option<Vec<Setting>> {
    if !crate::config::valid_script_id(id) {
        return None;
    }
    let path = crate::config::extensions_dir()?
        .join(id)
        .join("manifest.json");
    let bytes = std::fs::read(path).ok()?;
    Manifest::parse(&bytes)
        .ok()
        .map(|manifest| manifest.settings)
}

/// 控件前缀 → 字段下标;与 `ids::ext_field` 互为反查。
pub fn slot(base: &str) -> Option<usize> {
    let slot: usize = base.strip_prefix("ext-")?.parse().ok()?;
    (slot < MAX_FIELDS).then_some(slot)
}

#[cfg(test)]
#[path = "settings_ext_tests.rs"]
mod tests;

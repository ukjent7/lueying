//! 设置页的扩展授权视图模型:同意状态、人话能力表与按钮可用性。
//!
//! 平台无关;设置窗与 Linux E2E 共用。真正的哈希检查与记录读写在
//! `lueying_ext::manifest::consent` 与平台侧动作里。

/// 已安装的运行期扩展包;设置页只读清单,不加载 Wasm。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptEntry {
    pub id: String,
    pub name: String,
    /// 原生扩展(独立进程);同意文案与列表说明要更醒目。
    pub native: bool,
    /// 授权页上的人话能力表。
    pub items: Vec<String>,
    /// 当前同意状态。
    pub consent: ConsentState,
    /// 清单里的声明式设置;设置页按它渲染。
    pub settings: Vec<lueying_ext::manifest::Setting>,
    /// 类别;同类扩展同时只启用一个。
    pub category: Option<lueying_ext::manifest::Category>,
}

impl ScriptEntry {
    /// 不需要授权的包(纯外观或没有能力)用它构造。
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            native: false,
            items: Vec::new(),
            consent: ConsentState::Approved,
            settings: Vec::new(),
            category: None,
        }
    }
}

/// 一个包当前是否需要(重新)同意。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConsentState {
    /// 当前内容与能力已同意,或不需要同意。
    #[default]
    Approved,
    /// 首次安装/从未同意。
    Missing,
    /// 同意过,但包内容或能力变了;`added` 是新增能力的人话。
    Changed { added: Vec<String> },
}

/// 配置里只有一个扩展时返回它;多扩展有序列表由配置文件维护。
fn selected<'a>(extensions: &[String], scripts: &'a [ScriptEntry]) -> Option<&'a ScriptEntry> {
    let [id] = extensions else {
        return None;
    };
    scripts.iter().find(|script| &script.id == id)
}

/// 扩展授权那行的说明:能力清单与同意状态。
pub fn detail(extensions: &[String], scripts: &[ScriptEntry]) -> String {
    let Some(script) = selected(extensions, scripts) else {
        if extensions.is_empty() {
            return "选择运行期扩展后查看能力".to_string();
        }
        let mut pending: Vec<String> = Vec::new();
        for id in extensions {
            match scripts.iter().find(|script| &script.id == id) {
                Some(script) if script.consent != ConsentState::Approved => {
                    pending.push(script.name.clone());
                }
                Some(_) => {}
                None => pending.push(format!("{id}(未安装)")),
            }
        }
        if pending.is_empty() {
            return format!("已启用 {} 个,均已同意", extensions.len());
        }
        return format!("{} 个待处理:{}", pending.len(), pending.join("、"));
    };
    let items = script.items.join("、");
    let kind = if script.native {
        "原生扩展(独立进程,受沙箱限制);"
    } else {
        ""
    };
    match &script.consent {
        ConsentState::Approved if script.items.is_empty() => {
            format!("{}无需授权", kind)
        }
        ConsentState::Approved => format!("{kind}已同意:{items}"),
        ConsentState::Missing => format!("{kind}待同意:{items}"),
        ConsentState::Changed { added } if added.is_empty() => {
            format!("{kind}包已更新,需重新同意:{items}")
        }
        ConsentState::Changed { added } => {
            format!("{kind}包已更新,新增 {};需重新同意", added.join("、"))
        }
    }
}

/// 「同意」按钮是否可点:选中的包或列表里任一包需要(重新)同意时。
pub fn approve_enabled(extensions: &[String], scripts: &[ScriptEntry]) -> bool {
    extensions.iter().any(|id| {
        scripts
            .iter()
            .find(|script| &script.id == id)
            .is_some_and(|script| script.consent != ConsentState::Approved)
    })
}

/// 选择扩展时的确认文案。
pub fn prompt(script: &ScriptEntry) -> String {
    let mut text = format!("允许「{}」使用以下能力？\n\n", script.name);
    if script.native {
        text.push_str("此扩展包含原生可执行文件,以独立进程运行,受系统沙箱与资源限额约束。\n\n");
    }
    for item in &script.items {
        text.push_str("· ");
        text.push_str(item);
        text.push('\n');
    }
    if let ConsentState::Changed { added } = &script.consent
        && !added.is_empty()
    {
        text.push_str(&format!("\n本次新增:{}。\n", added.join("、")));
    }
    text.push_str("\n同意后保存配置并启用。");
    text
}

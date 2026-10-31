//! 扩展宿主的方法族:动作候选注入、命令执行与声明式设置的读写。
//!
//! 从 `server_request` 挪出:`Ext` 请求与动作候选共用这一层,管道分发
//! 只留编排。方法都拿 `&State` 操作,隐私门禁在各入口自行把关。

use super::*;

/// 当前组合的整串输入命中用户触发表时,把动作交给引擎注入候选。
/// 标题表由脚本线程维护:命令不存在(扩展没启用或已卸载)就不出动作候选。
pub(crate) fn sync_action(state: &State, s: &mut Session) {
    // 隐私字段下扩展层整个停用:不注入动作候选,也不给执行入口。
    if s.private {
        s.engine.set_action(None);
        return;
    }
    let action = s.engine.composition().and_then(|comp| {
        let raw = comp.raw.replace('\'', "");
        let command = state
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .triggers
            .iter()
            .find(|(trigger, _)| trigger == &raw)
            .map(|(_, command)| command.clone())?;
        let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
        let hub = hub.as_ref()?;
        let titles = hub.titles.lock().unwrap_or_else(|p| p.into_inner());
        let title = titles.get(&command)?.clone();
        Some(Action {
            trigger: raw,
            id: command,
            title,
        })
    });
    s.engine.set_action(action);
}

/// 把动作候选选中的命令交给脚本线程执行;扩展不在线时只写日志。
pub(crate) fn invoke_extension(state: &State, id: &str) {
    let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
    match hub.as_ref() {
        Some(hub) => {
            let _ = hub.invoke.send(ExtInvoke {
                command: id.to_string(),
                source: "动作候选",
            });
        }
        None => println!("动作候选跳过:脚本线程未启动({id})"),
    }
}

/// `Ext` 的 `commands.list`:标题表就是已加载命令的权威清单,供外部
/// 启动器枚举(PowerToys 命令面板书签、脚本、CLI `lueying ext commands`)。
pub(crate) fn commands_list(state: &State) -> Result<serde_json::Value, String> {
    let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
    let hub = hub.as_ref().ok_or("扩展线程未启动;先在设置里启用扩展")?;
    let titles = hub.titles.lock().unwrap_or_else(|p| p.into_inner());
    let mut list: Vec<(&String, &String)> = titles.iter().collect();
    list.sort();
    Ok(serde_json::Value::Array(
        list.into_iter()
            .map(|(id, title)| serde_json::json!({ "id": id, "title": title }))
            .collect(),
    ))
}

/// `Ext` 的 `command.run`:外部启动器触发扩展命令。与动作候选、热键走
/// 同一条执行路径,结果落在掠影自己的界面与日志;执行是异步的,这里只
/// 确认已投递。隐私字段下拒绝,与热键的门禁一致。
pub(crate) fn command_run(
    state: &State,
    sessions: &std::collections::HashMap<u64, Session>,
    id: &str,
) -> Result<serde_json::Value, String> {
    if sessions
        .get(&state.active.load(Ordering::Relaxed))
        .is_some_and(|session| session.private)
    {
        return Err("隐私字段下不执行扩展命令".into());
    }
    // 清单 id 一律小写;外部调用方大小写混用也认。
    let id = id.trim().to_ascii_lowercase();
    let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
    let hub = hub.as_ref().ok_or("扩展线程未启动;先在设置里启用扩展")?;
    {
        let titles = hub.titles.lock().unwrap_or_else(|p| p.into_inner());
        if !titles.contains_key(&id) {
            return Err(format!("命令 {id} 不存在;用 commands.list 查看可用命令"));
        }
    }
    hub.invoke
        .send(ExtInvoke {
            command: id.clone(),
            source: "外部触发",
        })
        .map_err(|_| "扩展线程已退出".to_string())?;
    Ok(serde_json::json!({ "submitted": true, "command": id }))
}

/// `Ext` 的 `settings.get`:清单默认值 + 配置里的当前值。
pub(crate) fn settings_get(state: &State, id: &str) -> Result<serde_json::Value, String> {
    let settings = lueying_server::settings_ext::manifest_settings(id)
        .ok_or_else(|| format!("扩展 {id} 未安装或清单不可读"))?;
    if settings.is_empty() {
        return Err(format!("扩展 {id} 没有声明式设置"));
    }
    let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
    Ok(lueying_server::settings_ext::values(
        id,
        &settings,
        cfg.ext_settings.get(id),
    ))
}

/// `Ext` 的 `settings.set`:逐键校验 schema,合并后落盘并下发。
pub(crate) fn settings_set(
    state: &State,
    sessions: &mut std::collections::HashMap<u64, Session>,
    id: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let settings = lueying_server::settings_ext::manifest_settings(id)
        .ok_or_else(|| format!("扩展 {id} 未安装或清单不可读"))?;
    if settings.is_empty() {
        return Err(format!("扩展 {id} 没有声明式设置"));
    }
    let serde_json::Value::Object(object) = params else {
        return Err("params 必须是对象,键是设置键".into());
    };
    if object.is_empty() {
        return Err("params 为空".into());
    }
    let mut table = state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .ext_settings
        .get(id)
        .cloned()
        .unwrap_or_default();
    for (key, value) in &object {
        let Some(setting) = settings
            .iter()
            .find(|setting| setting.key() == key.as_str())
        else {
            return Err(format!("扩展 {id} 没有设置 {key:?}"));
        };
        if !setting.accepts(value) {
            return Err(format!("设置 {key} 的值不符合清单 schema"));
        }
        if setting.is_secret() {
            let Some(text) = value.as_str() else {
                return Err(format!("密钥设置 {key} 必须是文本"));
            };
            lueying_server::settings_secret::set(id, key, text)?;
        } else {
            table.insert(key.clone(), value.clone());
        }
    }
    let full = lueying_server::settings_ext::values(id, &settings, Some(&table));
    let mut cfg = state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    cfg.ext_settings.insert(id.to_string(), table);
    if let Some(path) = &state.config_path
        && let Err(error) = config::save(path, &cfg)
    {
        return Err(format!("保存配置失败: {error}"));
    }
    apply_config(state, sessions, cfg);
    Ok(full)
}

//! 扩展宿主的方法族:动作候选注入与命令执行。
//!
//! 从 `server_request` 挪出:`Ext` 请求与动作候选共用这一层,管道分发
//! 只留编排。方法都拿 `&State` 操作,隐私门禁在各入口自行把关。

use super::*;

use lueying_core::Action;

/// 当前组合的整串输入命中用户触发表时,把动作交给引擎注入候选。
/// 标题表由宿主上报:命令不存在(扩展没启用或已卸载)就不出动作候选。
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
        let title = state
            .ext
            .titles()
            .into_iter()
            .find(|(id, _)| id == &command)
            .map(|(_, title)| title)?;
        Some(Action {
            trigger: raw,
            id: command,
            title,
        })
    });
    s.engine.set_action(action);
}

/// 把动作候选选中的命令交给扩展宿主执行;宿主不在线时消息留在队列里。
pub(crate) fn invoke_extension(state: &State, id: &str) {
    state.ext.invoke(id, "动作候选");
}

/// `Ext` 的 `commands.list`:标题表就是已加载命令的权威清单,供外部
/// 启动器枚举(PowerToys 命令面板书签、脚本、CLI `lueying ext commands`)。
pub(crate) fn commands_list(state: &State) -> Result<serde_json::Value, String> {
    let mut list = state.ext.titles();
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
pub(crate) fn command_run(state: &State, id: &str) -> Result<serde_json::Value, String> {
    if state
        .active_published()
        .is_some_and(|published| published.private)
    {
        return Err("隐私字段下不执行扩展命令".into());
    }
    // 清单 id 一律小写;外部调用方大小写混用也认。
    let id = id.trim().to_ascii_lowercase();
    if !state.ext.titles().iter().any(|(known, _)| known == &id) {
        return Err(format!("命令 {id} 不存在;用 commands.list 查看可用命令"));
    }
    state.ext.invoke(&id, "外部触发");
    Ok(serde_json::json!({ "submitted": true, "command": id }))
}

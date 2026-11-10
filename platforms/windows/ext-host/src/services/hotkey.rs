//! 全局热键:注册/冲突/注销;`press` 让扩展等一次触发。

use std::collections::HashMap;

use std::collections::VecDeque;

use std::sync::Mutex;

use lueying_ext::host::bindings::Modifier;

use lueying_ext::host::{Done, Provider};

use serde_json::json;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP, VkKeyScanW,
};

use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY};

/// 全局热键:注册/冲突/注销语义不变;`press` 让扩展等一次触发,用户绑定
/// 经 [`HotkeyProvider::bind`] 注册、[`HotkeyProvider::pump`] 回带触发行。
/// 所有注册与收取都发生在扩展线程上,RegisterHotKey 的线程亲和性成立。
#[derive(Default)]
pub struct HotkeyProvider {
    state: Mutex<HotkeyState>,
}

#[derive(Default)]
struct HotkeyState {
    /// 扩展热键 id → RegisterHotKey 的整数 id。
    map: HashMap<String, i32>,
    /// 用户绑定行号 → 整数 id。
    user: HashMap<usize, i32>,
    /// 扩展挂起的 press;每个热键事件兑现一个。
    presses: HashMap<String, VecDeque<Done>>,
    next: i32,
}

impl HotkeyState {
    fn take_id(&mut self) -> i32 {
        let id = self.next;
        self.next = if self.next >= 0xBFFF {
            1
        } else {
            self.next + 1
        };
        id
    }
}

impl Provider for HotkeyProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        match method {
            "register" => self.register(args, done),
            "unregister" => self.unregister(args, done),
            "press" => self.press(args, done),
            _ => Err(format!("hotkey 没有方法 {method:?}")),
        }
    }
}

impl HotkeyProvider {
    /// 注册一条用户绑定;行号在 [`HotkeyProvider::pump`] 的返回里原样带回。
    /// 组合串按绑定表语法校验(两个修饰键、保留组合),冲突报 OS 错误。
    pub fn bind(&self, row: usize, combo: &str) -> Result<(), String> {
        let (modifiers, key) = parse_combo(combo)?;
        let mut state = self.state.lock().unwrap();
        if let Some(&old) = state.user.get(&row) {
            let _ = unsafe { UnregisterHotKey(None, old) };
        }
        let hot_id = state.take_id();
        match unsafe { RegisterHotKey(None, hot_id, modifiers, key) } {
            Ok(()) => {
                state.user.insert(row, hot_id);
                Ok(())
            }
            Err(error) => Err(format!("热键注册失败: {error}")),
        }
    }

    /// 注销全部用户绑定;绑定表重建前调用,行号从零重排。
    pub fn unbind_users(&self) {
        let mut state = self.state.lock().unwrap();
        let ids = state.user.values().copied().collect::<Vec<_>>();
        for hot_id in ids {
            let _ = unsafe { UnregisterHotKey(None, hot_id) };
        }
        state.user.clear();
    }

    /// 注销全部热键并丢弃挂起的 press;运行期换扩展时整表重建。
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        let ids = state
            .map
            .values()
            .copied()
            .chain(state.user.values().copied())
            .collect::<Vec<_>>();
        for hot_id in ids {
            let _ = unsafe { UnregisterHotKey(None, hot_id) };
        }
        state.map.clear();
        state.user.clear();
        state.presses.clear();
    }

    /// 隐私字段出现时丢弃挂起的 press;扩展拿到明确拒绝,不留悬空 Promise。
    pub fn cancel_presses(&self, reason: &str) {
        let mut state = self.state.lock().unwrap();
        for (_, queued) in state.presses.drain() {
            for done in queued {
                done.complete(Err(reason.to_string()));
            }
        }
    }

    /// 收一次消息队列:扩展挂起的 press 就地兑现,用户绑定行号回带给
    /// 调用方派发。扩展线程每轮循环调一次;没有热键时只有一次 PeekMessage。
    pub fn pump(&self) -> Vec<usize> {
        let mut fired = Vec::new();
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message != WM_HOTKEY {
                    continue;
                }
                let hot_id = msg.wParam.0 as i32;
                let mut state = self.state.lock().unwrap();
                if let Some(row) = state
                    .user
                    .iter()
                    .find(|entry| *entry.1 == hot_id)
                    .map(|entry| *entry.0)
                {
                    fired.push(row);
                    continue;
                }
                let Some(id) = state
                    .map
                    .iter()
                    .find(|entry| *entry.1 == hot_id)
                    .map(|entry| entry.0.clone())
                else {
                    continue;
                };
                if let Some(queued) = state.presses.get_mut(&id) {
                    if let Some(done) = queued.pop_front() {
                        done.complete(Ok(json!({ "pressed": true }).to_string()));
                    }
                }
            }
        }
        fired
    }

    fn press(&self, args: &str, done: Done) -> Result<(), String> {
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"]
            .as_str()
            .ok_or_else(|| "缺 id".to_string())?
            .to_string();
        let mut state = self.state.lock().unwrap();
        if !state.map.contains_key(&id) {
            done.complete(Err(format!("热键未注册: {id}")));
            return Ok(());
        }
        state.presses.entry(id).or_default().push_back(done);
        Ok(())
    }

    fn register(&self, args: &str, done: Done) -> Result<(), String> {
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"]
            .as_str()
            .ok_or_else(|| "缺 id".to_string())?
            .to_string();
        let modifiers = modifiers(&parsed["modifiers"])?;
        let key = parsed["key"].as_str().ok_or_else(|| "缺 key".to_string())?;
        let virtual_key = virtual_key(key).ok_or_else(|| format!("不支持的热键 {key:?}"))?;
        let mut state = self.state.lock().unwrap();
        if state.map.contains_key(&id) {
            done.complete(Err(format!("热键已注册: {id}")));
            return Ok(());
        }
        let hot_id = state.take_id();
        match unsafe { RegisterHotKey(None, hot_id, modifiers, virtual_key) } {
            Ok(()) => {
                state.map.insert(id.clone(), hot_id);
                done.complete(Ok(json!({ "registered": true }).to_string()));
            }
            Err(error) => done.complete(Err(format!("热键注册失败: {error}"))),
        }
        Ok(())
    }

    fn unregister(&self, args: &str, done: Done) -> Result<(), String> {
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"]
            .as_str()
            .ok_or_else(|| "缺 id".to_string())?
            .to_string();
        let mut state = self.state.lock().unwrap();
        match state.map.remove(&id) {
            Some(hot_id) => {
                let result = unsafe { UnregisterHotKey(None, hot_id) }.map_err(|e| e.to_string());
                match result {
                    Ok(()) => done.complete(Ok(json!({ "unregistered": true }).to_string())),
                    Err(error) => done.complete(Err(error)),
                }
            }
            None => done.complete(Err(format!("热键未注册: {id}"))),
        }
        Ok(())
    }
}

/// `["ctrl","alt","shift","win"]` → MOD_*;未知修饰键报错。
fn modifiers(value: &serde_json::Value) -> Result<HOT_KEY_MODIFIERS, String> {
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    for modifier in value.as_array().into_iter().flatten() {
        match modifier.as_str() {
            Some("ctrl") => modifiers |= MOD_CONTROL,
            Some("alt") => modifiers |= MOD_ALT,
            Some("shift") => modifiers |= MOD_SHIFT,
            Some("win") => modifiers |= MOD_WIN,
            Some(other) => return Err(format!("不支持的修饰键 {other:?}")),
            None => return Err("修饰键必须是字符串数组".into()),
        }
    }
    Ok(modifiers)
}

/// 单个字符(经 VkKeyScanW)与常用命名键;更多命名键随 T4 的绑定表扩。
fn virtual_key(key: &str) -> Option<u32> {
    if let Some(number) = key
        .strip_prefix('F')
        .and_then(|rest| rest.parse::<u32>().ok())
        && (1..=12).contains(&number)
    {
        return Some(0x70 + number - 1);
    }
    let named = match key {
        "Space" => VK_SPACE,
        "Enter" => VK_RETURN,
        "Tab" => VK_TAB,
        "Backspace" => VK_BACK,
        "Delete" => VK_DELETE,
        "Insert" => VK_INSERT,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "Up" => VK_UP,
        "Down" => VK_DOWN,
        "Left" => VK_LEFT,
        "Right" => VK_RIGHT,
        "Esc" => VK_ESCAPE,
        _ => return single_character(key),
    };
    Some(named.0 as u32)
}

/// 单个字符:ASCII 字母数字直接映射,其余走当前键盘布局。
fn single_character(key: &str) -> Option<u32> {
    let mut chars = key.chars();
    let (first, second) = (chars.next(), chars.next());
    if second.is_some() {
        return None;
    }
    let character = first?;
    if character.is_ascii_alphanumeric() {
        return Some(character.to_ascii_uppercase() as u32);
    }
    if character == ' ' {
        return Some(VK_SPACE.0 as u32);
    }
    let scanned = unsafe { VkKeyScanW(character as u16) };
    if scanned == -1 {
        return None;
    }
    Some((scanned & 0xFF) as u32)
}

/// `Ctrl+Alt+O` → MOD_* 与虚拟键;语法、修饰键数量与保留组合走绑定表的解析。
fn parse_combo(combo: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let hotkey = lueying_ext::host::bindings::parse(combo)?;
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    for modifier in &hotkey.modifiers {
        modifiers |= match modifier {
            Modifier::Ctrl => MOD_CONTROL,
            Modifier::Alt => MOD_ALT,
            Modifier::Shift => MOD_SHIFT,
            Modifier::Win => MOD_WIN,
        };
    }
    let key = virtual_key(&hotkey.key).ok_or_else(|| format!("不支持的热键 {combo:?}"))?;
    Ok((modifiers, key))
}

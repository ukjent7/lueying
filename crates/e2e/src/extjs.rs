//! 运行期脚本扩展 E2E:包校验、场景协议、预算、隐私、热重载与命令运行期。
//!
//! 先列失败方式:
//! 1. 坏清单(未知字段、过高版本、声明能力、越界入口)被放进来。
//! 2. 顶层死循环或语法错拖住加载,输入法启动变慢或起不来。
//! 3. 状态里混进候选文字或宿主名,扩展能读到用户输入。
//! 4. 补丁未知字段、超长 SkSL、非法主题色被静默接受。
//! 5. handler 死循环或内存暴涨拖死进程,没有超时/限额。
//! 6. 隐私字段仍调用脚本;高对比度下效果没被摘掉。
//! 7. 热重载写坏文件后,正在用的扩展被弄没。
//! 8. 一次异常后宿主不可用,后续派发全挂。
//! 9. 卸载扩展后命令、触发词、热键还留在宿主里,或 JS 宿主没被丢弃。
//! 10. 未声明命令的扩展能被调到;多扩展补丁合并顺序乱掉。
//! 11. 未声明 needs 的服务调到了;缺提供者的服务静默成功;服务结果丢失。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lueying_extjs::bindings::{BindingTable, Input};
use lueying_extjs::{
    Done, Invoke, Limits, Pack, Poll, Provider, Registry, Runtime, SceneIn, Service, When,
};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

/// 临时目录;测试结束整棵删掉。
struct Temp {
    root: PathBuf,
}

impl Temp {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "lueying-extjs-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("建临时目录");
        Self { root }
    }

    fn pack(&self, dir: &str) -> PathBuf {
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

fn manifest(id: &str, api: u32) -> String {
    format!(r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":{api}}}"#)
}

fn manifest_extra(id: &str, api: u32, extra: &str) -> String {
    format!(r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":{api},{extra}}}"#)
}

fn write_pack(dir: &Path, manifest: &str, source: &str) {
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(dir.join("index.js"), source).expect("写脚本");
}

fn load_or_fail(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    dir: &Path,
) -> Option<Pack> {
    match Pack::load(dir) {
        Ok(pack) => Some(pack),
        Err(e) => {
            push(checks, name, false, e);
            None
        }
    }
}

const GOOD: &str = r#"
lueying.register(function (state) {
  return {
    accent: 0x123456,
    effects: {
      background: "half4 main(float2 p) { return half4(0.1, 0.2, 0.3, 0.5); }",
    },
  };
});
"#;

/// 用 `selected` 当命令通道,按不同下标返回不同坏补丁。
const BAD_PATCHES: &str = r#"
lueying.register(function (state) {
  if (state.selected === 1) return { accent: 0x1ffffff };
  if (state.selected === 2) return { effects: { background: "x".repeat(70000) } };
  if (state.selected === 3) return { future: true };
  return { accent: 0x00ff00 };
});
"#;

const COUNTER: &str = r#"
var calls = 0;
lueying.register(function () {
  calls += 1;
  return {
    accent: 0x100000 + calls,
    effects: { menu: "half4 main(float2 p) { return half4(0.0); }" },
  };
});
"#;

const THROW: &str = r#"
lueying.register(function (state) {
  if (state.selected === 7) throw new Error("boom");
  return { accent: 0x654321 };
});
"#;

const SPIN: &str = "lueying.register(function () { while (true) {} });";

const HOG: &str = "lueying.register(function () { var a = new ArrayBuffer(64 * 1024 * 1024); return { accent: 1 }; });";

/// 包校验:坏清单在加载期就拒绝,错误文本能定位到字段。
fn bad_packs(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("bad");
    let cases = [
        (
            "清单未知字段被拒",
            "unknown",
            manifest_extra("unknown", 1, r#""future":true"#),
            GOOD,
            "future",
        ),
        (
            "契约版本过高被拒",
            "apihigh",
            manifest("apihigh", 99),
            GOOD,
            "99",
        ),
        (
            "能力声明被拒",
            "cap",
            manifest_extra("cap", 1, r#""capabilities":["candidates"]"#),
            GOOD,
            "candidates",
        ),
        (
            "入口路径越界被拒",
            "escape",
            manifest_extra("escape", 1, r#""main":"../evil.js""#),
            GOOD,
            "入口脚本",
        ),
        (
            "目录名与 id 不一致被拒",
            "wrong",
            manifest("mismatch", 1),
            GOOD,
            "不一致",
        ),
        (
            "未注册 handler 被拒",
            "noreg",
            manifest("noreg", 1),
            "var x = 1;",
            "register",
        ),
        (
            "语法错误被拒",
            "syntax",
            manifest("syntax", 1),
            "function ( {",
            "SyntaxError",
        ),
    ];
    for (name, dir_name, manifest, source, needle) in cases {
        let dir = temp.pack(dir_name);
        write_pack(&dir, &manifest, source);
        let error = Pack::load(&dir).err().unwrap_or_default();
        push(
            checks,
            name,
            !error.is_empty() && error.contains(needle),
            error,
        );
    }

    let dir = temp.pack("spin");
    write_pack(&dir, &manifest("spin", 1), "while (true) {}");
    let limits = Limits {
        init_ms: 20,
        ..Limits::default()
    };
    let started = Instant::now();
    let error = Pack::load_with(&dir, limits).err().unwrap_or_default();
    let elapsed = started.elapsed();
    push(
        checks,
        "顶层死循环被初始化时限打断",
        error.contains("超时") && elapsed < Duration::from_secs(2),
        format!("{elapsed:?} {error}"),
    );
}

/// 正常包、状态字段与规整。
fn scene(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("scene");
    let dir = temp.pack("demo");
    write_pack(&dir, &manifest("demo", 1), GOOD);
    let Some(mut pack) = load_or_fail(checks, "正常包加载", &dir) else {
        return;
    };
    let patch = pack.dispatch(&SceneIn::default());
    let accent = patch.as_ref().and_then(|p| p.accent);
    let background = patch
        .as_ref()
        .and_then(|p| p.effects.as_ref())
        .and_then(|e| e.background.clone());
    push(
        checks,
        "正常包返回主题色与效果",
        accent == Some(0x123456) && background.as_deref().is_some_and(|s| s.contains("main")),
        format!("{accent:?} {background:?}"),
    );

    let state = SceneIn {
        candidates: 3,
        selected: 1,
        accent: 0x2f6fed,
        ..SceneIn::default()
    };
    let json = state.to_json().unwrap_or_default();
    let keys = [
        "api",
        "composing",
        "page",
        "pages",
        "candidates",
        "selected",
        "accent",
        "dark",
        "high_contrast",
        "reduce_motion",
        "time",
        "anim",
        "private",
    ];
    let fixed = keys.iter().all(|key| json.contains(&format!("\"{key}\":")));
    let no_text = !json.contains("你好") && !json.contains("text") && !json.contains("host");
    push(
        checks,
        "状态字段固定且不含文字/宿主",
        fixed && no_text,
        json,
    );

    let state = SceneIn {
        page: 9,
        pages: 3,
        candidates: 2,
        selected: 7,
        accent: 0x1ff_ffff,
        time: f32::NAN,
        anim: f32::INFINITY,
        ..SceneIn::default()
    };
    let json = state.to_json().unwrap_or_default();
    let clean = json.contains("\"page\":3")
        && json.contains("\"selected\":1")
        && json.contains("\"accent\":16777215")
        && json.contains("\"time\":0.0")
        && json.contains("\"anim\":1.0");
    push(checks, "越界状态被规整", clean, json);
}

/// 坏补丁:未知字段、超长源码、非法主题色都拒绝,好补丁能把计数清零。
fn patches(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("patch");
    let dir = temp.pack("patch");
    write_pack(&dir, &manifest("patch", 1), BAD_PATCHES);
    let Some(mut pack) = load_or_fail(checks, "坏补丁包加载", &dir) else {
        return;
    };
    let good = |selected: u32| SceneIn {
        candidates: 4,
        selected,
        ..SceneIn::default()
    };
    let ok = pack.dispatch(&good(0)).and_then(|p| p.accent).is_some();
    let bad_accent = pack.dispatch(&good(1)).is_none();
    let accent_error = pack.last_error().unwrap_or_default().to_string();
    let ok_again = pack.dispatch(&good(0)).and_then(|p| p.accent).is_some();
    let bad_size = pack.dispatch(&good(2)).is_none();
    let size_error = pack.last_error().unwrap_or_default().to_string();
    let ok_third = pack.dispatch(&good(0)).and_then(|p| p.accent).is_some();
    let bad_field = pack.dispatch(&good(3)).is_none();
    let field_error = pack.last_error().unwrap_or_default().to_string();
    push(
        checks,
        "非法主题色被拒",
        ok && bad_accent && accent_error.contains("主题色"),
        accent_error,
    );
    push(
        checks,
        "超长效果源码被拒",
        ok_again && bad_size && size_error.contains("超过"),
        size_error,
    );
    push(
        checks,
        "补丁未知字段被拒",
        ok_third && bad_field && field_error.contains("future"),
        field_error,
    );
}

/// 预算:handler 死循环被中断,连续失败后停用。
fn budget(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("budget");
    let dir = temp.pack("spin");
    write_pack(&dir, &manifest("spin", 1), SPIN);
    let limits = Limits {
        dispatch_ms: 20,
        ..Limits::default()
    };
    let Some(mut pack) = Pack::load_with(&dir, limits).ok() else {
        push(checks, "死循环包加载", false, "加载失败".into());
        return;
    };
    let started = Instant::now();
    let first = pack.dispatch(&SceneIn::default());
    let elapsed = started.elapsed();
    let error = pack.last_error().unwrap_or_default().to_string();
    let _ = pack.dispatch(&SceneIn::default());
    let _ = pack.dispatch(&SceneIn::default());
    let disabled = pack.disabled_reason().is_some();
    let after = pack.dispatch(&SceneIn::default());
    push(
        checks,
        "handler 死循环被派发时限打断",
        first.is_none() && error.contains("超时") && elapsed < Duration::from_secs(2),
        format!("{elapsed:?} {error}"),
    );
    push(
        checks,
        "连续失败后停用扩展",
        disabled && after.is_none(),
        format!("{disabled}"),
    );

    let dir = temp.pack("hog");
    write_pack(&dir, &manifest("hog", 1), HOG);
    // 派发预算放宽,让内存限额先触发;否则在慢机器上可能先撞超时。
    let limits = Limits {
        dispatch_ms: 500,
        ..Limits::default()
    };
    let Some(mut pack) = Pack::load_with(&dir, limits).ok() else {
        push(checks, "内存包加载", false, "加载失败".into());
        return;
    };
    let hog = pack.dispatch(&SceneIn::default());
    let error = pack.last_error().unwrap_or_default().to_string();
    push(
        checks,
        "内存暴涨被运行时限额拦住",
        hog.is_none() && error.contains("内存"),
        error,
    );
}

/// 隐私、高对比度与异常后的可用性。
fn privacy(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("privacy");
    let dir = temp.pack("counter");
    write_pack(&dir, &manifest("counter", 1), COUNTER);
    let Some(mut pack) = load_or_fail(checks, "计数包加载", &dir) else {
        return;
    };
    let private = pack.dispatch(&SceneIn {
        private: true,
        ..SceneIn::default()
    });
    let normal = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    let high = pack.dispatch(&SceneIn {
        high_contrast: true,
        ..SceneIn::default()
    });
    let high_accent = high.as_ref().and_then(|p| p.accent);
    let high_effects = high.as_ref().is_some_and(|p| p.effects.as_ref().is_none());
    push(
        checks,
        "隐私字段不调用脚本",
        private.is_none() && normal == Some(0x100001),
        format!("{private:?} {normal:?}"),
    );
    push(
        checks,
        "高对比度摘掉补丁效果",
        high_accent == Some(0x100002) && high_effects,
        format!("{high_accent:?}"),
    );

    let dir = temp.pack("throw");
    write_pack(&dir, &manifest("throw", 1), THROW);
    let Some(mut pack) = load_or_fail(checks, "异常包加载", &dir) else {
        return;
    };
    let boom = pack.dispatch(&SceneIn {
        candidates: 8,
        selected: 7,
        ..SceneIn::default()
    });
    let error = pack.last_error().unwrap_or_default().to_string();
    let after = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    push(
        checks,
        "一次异常后宿主仍可派发",
        boom.is_none() && error.contains("boom") && after == Some(0x654321),
        error,
    );
}

/// 热重载:换新、写坏保留上一版、删除清单后卸载。
fn reload(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("reload");
    let dir = temp.pack("hot");
    let v1 = r#"lueying.register(function () { return { accent: 0x111111 }; });"#;
    let v2 = r#"lueying.register(function () { return { accent: 0x222222 }; });"#;
    let v3 = r#"lueying.register(function () { return { accent: 0x333333 }; });"#;
    write_pack(&dir, &manifest("hot", 1), v1);
    let Some(mut pack) = load_or_fail(checks, "热重载包加载", &dir) else {
        return;
    };
    let a1 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    std::fs::write(dir.join("index.js"), v2).expect("写 V2");
    let polled = pack.poll();
    let a2 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    std::fs::write(dir.join("index.js"), "function ( {").expect("写坏脚本");
    let broken = pack.poll();
    let a3 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    std::fs::write(dir.join("index.js"), v3).expect("写 V3");
    let fixed = pack.poll();
    let a4 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    push(
        checks,
        "热重载换新且写坏时保留上一版",
        a1 == Some(0x111111)
            && polled == Poll::Reloaded
            && a2 == Some(0x222222)
            && matches!(&broken, Poll::Failed(_))
            && a3 == Some(0x222222)
            && fixed == Poll::Reloaded
            && a4 == Some(0x333333),
        format!("{a1:?} {polled:?} {a2:?} {broken:?} {a3:?} {fixed:?} {a4:?}"),
    );
    std::fs::remove_file(dir.join("manifest.json")).expect("删清单");
    let removed = pack.poll();
    push(
        checks,
        "清单删除后卸载",
        removed == Poll::Removed && pack.dispatch(&SceneIn::default()).is_none(),
        format!("{removed:?}"),
    );
}

/// 新清单:外观组件可加载;逻辑-only 包不进外观宿主;多组件包只取外观。
fn manifests(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("manifests");

    let dir = temp.pack("appearance");
    let manifest = r#"{"id":"appearance","name":"测试","version":"1.0.0","appearance":{"api":1,"main":"index.js"}}"#;
    write_pack(&dir, manifest, GOOD);
    if let Some(mut pack) = load_or_fail(checks, "新清单外观组件可加载", &dir) {
        let accent = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
        push(
            checks,
            "新清单外观组件可派发",
            accent == Some(0x123456),
            format!("{accent:?}"),
        );
    }

    let dir = temp.pack("multi");
    let manifest = r#"{"id":"multi","name":"测试","version":"1.0.0","appearance":{"api":1},"logic":{"api":1,"module":"logic.wasm"}}"#;
    write_pack(&dir, manifest, GOOD);
    if let Some(mut pack) = load_or_fail(checks, "多组件包外观部分可加载", &dir) {
        let accent = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
        push(
            checks,
            "多组件包只取外观组件",
            accent == Some(0x123456),
            format!("{accent:?}"),
        );
    }

    let dir = temp.pack("logic-only");
    let manifest = r#"{"id":"logic-only","name":"测试","version":"1.0.0","logic":{"api":1,"module":"logic.wasm"}}"#;
    write_pack(&dir, manifest, GOOD);
    let error = Pack::load(&dir).err();
    push(
        checks,
        "逻辑包不进脚本宿主",
        error.as_deref().is_some_and(|e| e.contains("脚本入口")),
        format!("{error:?}"),
    );
}

/// 多扩展运行期:有序加载、懒加载、命令作用域、空闲卸载与 Scope 回收。
fn runtime(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("runtime");

    let command_manifest = |id: &str| {
        format!(
            r#"{{"id":"{id}","name":"命令包","version":"1.0.0","api":2,"main":"index.js","activate":{{"commands":[{{"id":"run","title":"运行"}}]}},"needs":["notify"]}}"#
        )
    };
    let command_source = r#"
lueying.command("run", function (args) {
  return { ok: true, echo: args ? args.value : 0 };
});
"#;
    let a = temp.pack("alpha");
    write_pack(&a, &command_manifest("alpha"), command_source);
    let b = temp.pack("beta");
    write_pack(&b, &command_manifest("beta"), command_source);

    let order = vec!["alpha".to_string(), "beta".to_string()];
    let (mut runtime, errors) = Runtime::open(&temp.root, &order, Limits::default());
    let commands = runtime.commands();
    let listed = commands.len() == 2
        && commands.iter().any(|command| command.id == "alpha.run")
        && commands.iter().any(|command| command.id == "beta.run");
    push(
        checks,
        "多扩展命令按清单列出且不加载 JS",
        errors.is_empty() && listed && runtime.loaded_count() == 0,
        format!(
            "{errors:?} {:?} loaded={}",
            commands,
            runtime.loaded_count()
        ),
    );

    let invoked = runtime.invoke("alpha.run", r#"{"value":7}"#);
    let only_alpha = runtime.loaded("alpha") && !runtime.loaded("beta");
    push(
        checks,
        "命令按需加载且只拉起被调用的扩展",
        matches!(&invoked, Ok(Invoke::Done(text)) if text == r#"{"ok":true,"echo":7}"#)
            && only_alpha,
        format!(
            "{invoked:?} alpha={} beta={}",
            runtime.loaded("alpha"),
            runtime.loaded("beta")
        ),
    );

    let scope = runtime.scope("alpha");
    push(
        checks,
        "Scope 暴露声明的命令",
        scope
            .as_ref()
            .is_some_and(|scope| scope.commands == vec!["run"]),
        format!("{scope:?}"),
    );

    let unloaded = runtime.unload("alpha");
    let gone = runtime.scope("alpha").is_none()
        && runtime
            .commands()
            .iter()
            .all(|command| command.id != "alpha.run")
        && runtime.invoke("alpha.run", "").is_err();
    push(
        checks,
        "卸载后 Scope 与命令全部消失",
        unloaded && gone,
        format!("unloaded={unloaded} commands={:?}", runtime.commands()),
    );

    let beta = runtime.invoke("beta.run", "");
    runtime.set_idle(Duration::ZERO);
    runtime.idle_unload();
    let still_listed = runtime
        .commands()
        .iter()
        .any(|command| command.id == "beta.run");
    push(
        checks,
        "空闲卸载丢弃 JS 但保留命令声明",
        beta.is_ok() && !runtime.loaded("beta") && still_listed,
        format!(
            "{beta:?} loaded={} listed={still_listed}",
            runtime.loaded("beta")
        ),
    );

    let first = temp.pack("first");
    write_pack(
        &first,
        &manifest("first", 1),
        r#"lueying.register(function () { return { accent: 0x111111 }; });"#,
    );
    let second = temp.pack("second");
    write_pack(
        &second,
        &manifest("second", 1),
        r#"lueying.register(function () { return { accent: 0x222222 }; });"#,
    );
    let (mut forward, _) = Runtime::open(
        &temp.root,
        &["first".to_string(), "second".to_string()],
        Limits::default(),
    );
    let forward_accent = forward
        .dispatch(&SceneIn::default())
        .and_then(|patch| patch.accent);
    let (mut backward, _) = Runtime::open(
        &temp.root,
        &["second".to_string(), "first".to_string()],
        Limits::default(),
    );
    let backward_accent = backward
        .dispatch(&SceneIn::default())
        .and_then(|patch| patch.accent);
    push(
        checks,
        "外观补丁按配置顺序合并",
        forward_accent == Some(0x222222) && backward_accent == Some(0x111111),
        format!("{forward_accent:?} {backward_accent:?}"),
    );

    let (mut missing, errors) = Runtime::open(
        &temp.root,
        &["ghost".to_string(), "first".to_string()],
        Limits::default(),
    );
    let usable = missing.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    push(
        checks,
        "坏扩展只记诊断,不影响其他扩展",
        !errors.is_empty() && usable == Some(0x111111),
        format!("{errors:?} {usable:?}"),
    );
}

/// 异步命令:Promise 挂起、宿主时钟推进、步进预算与失败结算。
fn async_commands(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("async");
    let manifest = r#"{"id":"async","name":"异步","version":"1.0.0","api":2,"main":"index.js","activate":{"commands":[{"id":"slow","title":"慢"},{"id":"spin","title":"转"},{"id":"boom","title":"炸"}]}}"#;
    const SOURCE: &str = r#"
lueying.command("slow", function (args) {
  return lueying.timeout(args.ms).then(function () {
    return { done: true };
  });
});
lueying.command("spin", function () {
  return lueying.timeout(1).then(function () { while (true) {} });
});
lueying.command("boom", function () {
  return lueying.timeout(1).then(function () { throw new Error("late boom"); });
});
"#;
    let dir = temp.pack("async");
    write_pack(&dir, manifest, SOURCE);
    let limits = Limits {
        dispatch_ms: 20,
        ..Limits::default()
    };
    let (mut runtime, errors) = Runtime::open(&temp.root, &["async".to_string()], limits);
    if !errors.is_empty() {
        push(checks, "异步包打开", false, format!("{errors:?}"));
        return;
    }

    let slow = runtime.invoke("async.slow", r#"{"ms":50}"#);
    let pending = matches!(&slow, Ok(Invoke::Pending(1)));
    let started = Instant::now();
    let early = runtime.pump(0);
    let pump_elapsed = started.elapsed();
    push(
        checks,
        "Promise 命令挂起且泵不空转",
        pending && early.is_empty() && pump_elapsed < Duration::from_secs(1),
        format!("{slow:?} {early:?} {pump_elapsed:?}"),
    );
    push(
        checks,
        "等待期不结算",
        runtime.pump(49).is_empty() && runtime.pending() == 1,
        format!("pending={}", runtime.pending()),
    );
    let settled = runtime.pump(50);
    push(
        checks,
        "到点后结算为 JSON",
        settled.len() == 1
            && settled[0].0 == "async"
            && settled[0].1.token == 1
            && settled[0].1.ok
            && settled[0].1.value.as_deref() == Some(r#"{"done":true}"#),
        format!("{settled:?}"),
    );

    let spin = runtime.invoke("async.spin", "");
    let started = Instant::now();
    // slow 段结束时时钟已推进到 50,spin 的定时器到点在 51,合成时钟要越过它。
    // 被杀作业的 Promise 不结算(quickjs 对不可捕获异常跳过 capability
    // reject),两泵都应空,宿主把挂起记账清零。
    let interrupted = runtime.pump(60);
    let continued = runtime.pump(60);
    let spin_elapsed = started.elapsed();
    push(
        checks,
        "死循环作业被步进预算打断且记账清零",
        matches!(spin, Ok(Invoke::Pending(_)))
            && interrupted.is_empty()
            && continued.is_empty()
            && runtime.pending() == 0
            && spin_elapsed < Duration::from_secs(2),
        format!("{spin:?} {interrupted:?} {continued:?} {spin_elapsed:?}"),
    );

    let boom = runtime.invoke("async.boom", "");
    // boom 在被杀之后注册,定时器到点在 61,用更大的合成时钟越过它;
    // 能正常结算说明中断后宿主仍然可用。
    let settled = runtime.pump(200);
    let error = settled
        .iter()
        .find(|(_, item)| !item.ok)
        .and_then(|(_, item)| item.error.clone())
        .unwrap_or_default();
    push(
        checks,
        "异步异常带错误文本结算",
        matches!(boom, Ok(Invoke::Pending(3))) && error.contains("late boom"),
        format!("{boom:?} {error:?}"),
    );
    push(
        checks,
        "全部任务结算后挂起清零",
        runtime.pending() == 0,
        format!("pending={}", runtime.pending()),
    );
}

/// 服务缝的假提供者:截图写临时文件,识别返回固定文本。
struct FakeCapture {
    dir: PathBuf,
}

impl Provider for FakeCapture {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        let _ = args;
        if method != "shot" {
            return Err(format!("假 capture 不认识方法 {method}"));
        }
        let file = self.dir.join("shot.bmp");
        std::fs::write(&file, b"BMfake").map_err(|e| e.to_string())?;
        let payload = serde_json::json!({
            "file": file.to_string_lossy(),
            "width": 80,
            "height": 20,
        });
        done.complete(Ok(payload.to_string()));
        Ok(())
    }
}

/// 假识别:参数里缺 file 时经完成队列回错误文本。
struct FakeOcr;

impl Provider for FakeOcr {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        if method != "recognize" {
            return Err(format!("假 ocr 不认识方法 {method}"));
        }
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        if parsed["file"].as_str().unwrap_or_default().is_empty() {
            done.complete(Err("缺 file".into()));
            return Ok(());
        }
        done.complete(Ok(r#"{"text":"假识别文本","lang":"zh"}"#.into()));
        Ok(())
    }
}

/// 假上屏:记录送进来的文本,断言链路时核对。
#[derive(Default)]
struct FakeOutput {
    delivered: Mutex<Vec<String>>,
}

impl Provider for FakeOutput {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        if method != "deliver" {
            return Err(format!("假 output 不认识方法 {method}"));
        }
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        self.delivered
            .lock()
            .unwrap()
            .push(parsed["text"].as_str().unwrap_or_default().to_string());
        done.complete(Ok(r#"{"delivered":true}"#.into()));
        Ok(())
    }
}

/// 假面板:先挂住调用,测试调 settle 才完成,验证跨泵轮的异步结算。
struct FakePanel {
    waiting: Mutex<Option<Done>>,
}

impl FakePanel {
    fn settle(&self, selected: Option<u32>) {
        if let Some(done) = self.waiting.lock().unwrap().take() {
            let payload = match selected {
                Some(index) => format!(r#"{{"selected":{index}}}"#),
                None => r#"{"selected":null}"#.to_string(),
            };
            done.complete(Ok(payload));
        }
    }
}

impl Provider for FakePanel {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        let _ = args;
        if method != "show" {
            return Err(format!("假 panel 不认识方法 {method}"));
        }
        *self.waiting.lock().unwrap() = Some(done);
        Ok(())
    }
}

/// 假热键:重复注册走完成队列的错误分支。
#[derive(Default)]
struct FakeHotkey {
    registered: Mutex<Vec<String>>,
}

impl Provider for FakeHotkey {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        if method != "register" {
            return Err(format!("假 hotkey 不认识方法 {method}"));
        }
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let id = parsed["id"].as_str().unwrap_or_default().to_string();
        let mut ids = self.registered.lock().unwrap();
        if ids.contains(&id) {
            done.complete(Err(format!("热键已注册: {id}")));
            return Ok(());
        }
        ids.push(id);
        done.complete(Ok(r#"{"registered":true}"#.into()));
        Ok(())
    }
}

/// 服务缝:needs 门控、调用路由、完成队列与错误回传。
/// notify 故意不装提供者:验证「声明了但提供者缺席」的报错路径。
fn services(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("services");
    let shots = temp.root.join("shots");
    std::fs::create_dir_all(&shots).expect("建截图目录");
    let svc_manifest = r#"{"id":"svc","name":"服务","version":"1.0.0","api":2,"main":"index.js","needs":["capture","ocr","output","panel","hotkey","notify"],"activate":{"commands":[{"id":"chain","title":"链"},{"id":"panel","title":"板"},{"id":"hk","title":"键"},{"id":"notify","title":"告"}]}}"#;
    let gate_manifest = r#"{"id":"gate","name":"门","version":"1.0.0","api":2,"main":"index.js","needs":[],"activate":{"commands":[{"id":"try_ocr","title":"试"}]}}"#;
    const SVC: &str = r#"
lueying.command("chain", function () {
  return lueying.capture.shot({}).then(function (shot) {
    return lueying.ocr.recognize({ file: shot.file });
  }).then(function (ocr) {
    return lueying.output.deliver({ text: ocr.text });
  }).then(function (delivered) {
    return { chain: true, delivered: delivered.delivered };
  });
});
lueying.command("panel", function () {
  return lueying.panel.show({ title: "测试", items: ["甲", "乙"] });
});
lueying.command("hk", function (args) {
  return lueying.hotkey.register({ id: args.id, modifiers: ["ctrl"], key: "K" });
});
lueying.command("notify", function () {
  return lueying.notify.show({ title: "标题", body: "正文" });
});
"#;
    const GATE: &str = r#"
lueying.command("try_ocr", function () {
  return lueying.ocr.recognize({ file: "x" });
});
"#;
    let svc_dir = temp.pack("svc");
    write_pack(&svc_dir, svc_manifest, SVC);
    let gate_dir = temp.pack("gate");
    write_pack(&gate_dir, gate_manifest, GATE);

    let output = Arc::new(FakeOutput::default());
    let panel = Arc::new(FakePanel {
        waiting: Mutex::new(None),
    });
    let registry = Arc::new(Registry::new());
    registry.set(Service::Capture, Arc::new(FakeCapture { dir: shots }));
    registry.set(Service::Ocr, Arc::new(FakeOcr));
    registry.set(Service::Output, output.clone());
    registry.set(Service::Panel, panel.clone());
    registry.set(Service::Hotkey, Arc::new(FakeHotkey::default()));
    let order = vec!["svc".to_string(), "gate".to_string()];
    let (mut runtime, errors) = Runtime::open_with(&temp.root, &order, Limits::default(), registry);
    if !errors.is_empty() {
        push(checks, "服务包打开", false, format!("{errors:?}"));
        return;
    }

    let chain = runtime.invoke("svc.chain", "");
    let mut settled = Vec::new();
    for _ in 0..10 {
        settled = runtime.pump(0);
        if !settled.is_empty() {
            break;
        }
    }
    push(
        checks,
        "服务链 capture→ocr→output",
        matches!(chain, Ok(Invoke::Pending(_)))
            && settled.len() == 1
            && settled[0].1.ok
            && settled[0].1.value.as_deref() == Some(r#"{"chain":true,"delivered":true}"#)
            && output
                .delivered
                .lock()
                .unwrap()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                == vec!["假识别文本"],
        format!("{chain:?} {settled:?}"),
    );

    let shown = runtime.invoke("svc.panel", "");
    let idle = runtime.pump(0);
    panel.settle(Some(0));
    let picked = runtime.pump(0);
    push(
        checks,
        "panel 等提供者再结算",
        matches!(shown, Ok(Invoke::Pending(_)))
            && idle.is_empty()
            && picked.len() == 1
            && picked[0].1.ok
            && picked[0].1.value.as_deref() == Some(r#"{"selected":0}"#),
        format!("{shown:?} {idle:?} {picked:?}"),
    );

    let first = runtime.invoke("svc.hk", r#"{"id":"paste"}"#);
    let first_settled = runtime.pump(0);
    let second = runtime.invoke("svc.hk", r#"{"id":"paste"}"#);
    let second_settled = runtime.pump(0);
    push(
        checks,
        "热键注册成功与冲突",
        matches!(first, Ok(Invoke::Pending(_)))
            && first_settled.len() == 1
            && first_settled[0].1.ok
            && matches!(second, Ok(Invoke::Pending(_)))
            && second_settled.len() == 1
            && !second_settled[0].1.ok
            && second_settled[0].1.error.as_deref() == Some("热键已注册: paste"),
        format!("{first:?} {first_settled:?} {second:?} {second_settled:?}"),
    );

    let notify = runtime.invoke("svc.notify", "");
    let settled = runtime.pump(0);
    push(
        checks,
        "缺提供者的服务报错",
        matches!(notify, Ok(Invoke::Pending(_)))
            && settled.len() == 1
            && !settled[0].1.ok
            && settled[0]
                .1
                .error
                .as_deref()
                .is_some_and(|error| error.contains("没有提供者")),
        format!("{notify:?} {settled:?}"),
    );

    let denied = runtime.invoke("gate.try_ocr", "");
    let settled = runtime.pump(0);
    push(
        checks,
        "未声明的服务被拒",
        matches!(denied, Ok(Invoke::Pending(_)))
            && settled.len() == 1
            && !settled[0].1.ok
            && settled[0]
                .1
                .error
                .as_deref()
                .is_some_and(|error| error.contains("未在 needs 声明")),
        format!("{denied:?} {settled:?}"),
    );

    push(
        checks,
        "服务结算后挂起清零",
        runtime.pending() == 0,
        format!("pending={}", runtime.pending()),
    );
}

/// 用户绑定表(T4 入口):解析归一、语法与保留组合拒绝、表内冲突、when 过滤。
fn hotkey_bindings(checks: &mut Vec<(&'static str, bool, String)>) {
    let parse = |combo: &str| lueying_extjs::bindings::parse(combo);
    let canonical = parse("alt+CTRL+o")
        .map(|hotkey| hotkey.canonical())
        .ok();
    push(
        checks,
        "组合串归一",
        canonical.as_deref() == Some("Ctrl+Alt+O"),
        format!("{canonical:?}"),
    );
    for (combo, why) in [
        ("Ctrl+O", "单修饰键"),
        ("Ctrl+Alt+O+P", "多主键"),
        ("Ctrl+Ctrl+O", "重复修饰键"),
        ("Ctrl+Shift+F13", "F 键越界"),
    ] {
        let error = parse(combo).err().unwrap_or_default();
        push(
            checks,
            "语法拒绝",
            !error.is_empty(),
            format!("{why}:{error:?}"),
        );
    }
    let mut reserved_ok = true;
    let mut reserved_note = String::new();
    for combo in [
        "Ctrl+Alt+Del",
        "Ctrl+Shift+Esc",
        "Ctrl+Win+Space",
        "Ctrl+Win+Left",
    ] {
        match parse(combo) {
            Ok(_) => {
                reserved_ok = false;
                reserved_note.push_str(combo);
            }
            Err(error) => reserved_note.push_str(&error),
        }
    }
    push(checks, "保留组合被拒", reserved_ok, reserved_note);
    let mut table = BindingTable::default();
    table
        .insert("Ctrl+Alt+O", "shot.run".to_string(), When::Any)
        .unwrap();
    let conflict = table
        .insert("alt+ctrl+o", "clip.search".to_string(), When::Any)
        .err();
    push(
        checks,
        "表内冲突带占用者",
        conflict.as_deref() == Some("热键 Ctrl+Alt+O 已绑定 shot.run"),
        conflict.unwrap_or_default(),
    );
    let mut table = BindingTable::default();
    table
        .insert("Ctrl+Alt+C", "shot.run".to_string(), When::Composing)
        .unwrap();
    table
        .insert("Ctrl+Alt+E", "clip.search".to_string(), When::Editable)
        .unwrap();
    let hit = table
        .hit(
            "ctrl+alt+c",
            Input {
                editable: true,
                composing: true,
            },
        )
        .map(|binding| binding.command.as_str());
    push(
        checks,
        "when 过滤:组句态命中 composing",
        hit == Some("shot.run"),
        format!("{hit:?}"),
    );
    let missed = table.hit(
        "ctrl+alt+c",
        Input {
            editable: true,
            composing: false,
        },
    );
    push(
        checks,
        "when 过滤:非组句态放过 composing",
        missed.is_none(),
        "None".to_string(),
    );
    let hit = table
        .hit(
            "ctrl+alt+e",
            Input {
                editable: true,
                composing: false,
            },
        )
        .map(|binding| binding.command.as_str());
    push(
        checks,
        "when 过滤:可编辑态命中 editable",
        hit == Some("clip.search"),
        format!("{hit:?}"),
    );
}

/// 运行全部检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    bad_packs(&mut checks);
    manifests(&mut checks);
    runtime(&mut checks);
    async_commands(&mut checks);
    services(&mut checks);
    hotkey_bindings(&mut checks);
    scene(&mut checks);
    patches(&mut checks);
    budget(&mut checks);
    privacy(&mut checks);
    reload(&mut checks);

    report.push_str("\n## 运行期脚本扩展\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0usize;
    for (name, ok, note) in &checks {
        if !ok {
            failures += 1;
        }
        let note = note.replace('|', "\\|").replace('\n', " ");
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if *ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}

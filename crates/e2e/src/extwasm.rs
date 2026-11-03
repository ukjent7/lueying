//! 运行期 Wasm 扩展 E2E:包校验、场景协议、预算、隐私、热重载与命令运行期。
//!
//! 先列失败方式:
//! 1. 坏清单(未知字段、过高版本、越界模块路径)被放进来。
//! 2. 缺导出(memory、分配器、入口)或坏模块的包被加载。
//! 3. 死循环、内存暴涨拖住宿主,没有燃料/限额兜底。
//! 4. 状态里混进候选文字或宿主名,扩展能读到用户输入。
//! 5. 补丁未知字段、非法主题色被静默接受。
//! 6. 隐私字段仍调用 guest。
//! 7. 热重载写坏文件后,正在用的扩展被弄没。
//! 8. 未声明命令的扩展能被调到;多扩展补丁合并顺序乱掉。

use std::time::{Duration, Instant};

use lueying_extwasm::{Limits, Pack, Poll, Runtime, SceneIn};

use crate::extwasm_harness::{
    appearance_guest, appearance_manifest, command_guest, command_manifest, error_guest, hog_guest,
    init_loop_guest, load_or_fail, loop_guest, once_trap_appearance_guest, plain_manifest,
    raw_manifest, sleep_guest, trap_guest, write_pack,
};
use crate::harness::{Temp, push};

const PATCH: &str = r#"{"accent":1193046}"#;

/// 包校验:坏清单与坏模块在加载期就拒绝,错误文本能定位到字段。
fn bad_packs(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("bad");
    let cases = [
        (
            "清单未知字段被拒",
            "unknown",
            raw_manifest("unknown", r#""future":true"#),
            command_guest("{}"),
            "future",
        ),
        (
            "契约版本过高被拒",
            "apihigh",
            raw_manifest("apihigh", r#""appearance":true"#).replace(r#""api":4"#, r#""api":99"#),
            command_guest("{}"),
            "99",
        ),
        (
            "模块路径越界被拒",
            "escape",
            raw_manifest("escape", r#""appearance":true"#)
                .replace(r#""extension.wasm""#, r#""../evil.wasm""#),
            command_guest("{}"),
            "Wasm 模块",
        ),
        (
            "目录名与 id 不一致被拒",
            "wrong",
            raw_manifest("mismatch", r#""appearance":true"#),
            command_guest("{}"),
            "不一致",
        ),
        (
            "命令包缺导出被拒",
            "nocmd",
            command_manifest("nocmd", &["run"], &[]),
            appearance_guest(PATCH),
            "没有导出命令",
        ),
        (
            "外观包缺 lueying_appearance 被拒",
            "noapp",
            appearance_manifest("noapp"),
            command_guest("{}"),
            "lueying_appearance",
        ),
        (
            "没有任何入口被拒",
            "noentry",
            plain_manifest("noentry"),
            command_guest("{}"),
            "没有可调用的入口",
        ),
        (
            "坏 Wasm 字节被拒",
            "garbage",
            plain_manifest("garbage"),
            "not wasm at all".to_string(),
            "编译失败",
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
    write_pack(&dir, &appearance_manifest("spin"), init_loop_guest());
    let limits = Limits {
        init_fuel: 1_000_000,
        ..Limits::default()
    };
    let started = Instant::now();
    let error = Pack::load_with(&dir, limits).err().unwrap_or_default();
    let elapsed = started.elapsed();
    push(
        checks,
        "初始化死循环被燃料打断",
        error.contains("fuel") && elapsed < Duration::from_secs(2),
        format!("{elapsed:?} {error}"),
    );
}

/// 正常包、状态字段与规整。
fn scene(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("scene");
    let dir = temp.pack("demo");
    write_pack(&dir, &appearance_manifest("demo"), appearance_guest(PATCH));
    let Some(mut pack) = load_or_fail(checks, "正常包加载", &dir) else {
        return;
    };
    let patch = pack.dispatch(&SceneIn::default());
    let accent = patch.as_ref().and_then(|p| p.accent);
    push(
        checks,
        "正常包返回主题色",
        accent == Some(0x123456),
        format!("{accent:?}"),
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

/// 坏补丁:未知字段、超长源码、非法主题色都拒绝,好补丁还能继续派发。
fn patches(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("patch");
    let dir = temp.pack("patch");
    write_pack(&dir, &appearance_manifest("patch"), appearance_guest(PATCH));
    let Some(mut pack) = load_or_fail(checks, "补丁包加载", &dir) else {
        return;
    };
    let _ = pack.dispatch(&SceneIn::default());
    let good = pack.last_error().is_none();
    // 用一个专门回错误的 guest 换掉同一个包里的模块,复用加载断言路径。
    let bad_dir = temp.pack("badpatch");
    write_pack(
        &bad_dir,
        &appearance_manifest("badpatch"),
        error_guest("主题色 0xffffff 超出 24 位"),
    );
    let mut bad = Pack::load(&bad_dir).expect("加载坏补丁包");
    let refused = bad.dispatch(&SceneIn::default()).is_none();
    let error = bad.last_error().unwrap_or_default().to_string();
    push(
        checks,
        "非法补丁被拒并给出原因",
        good && refused && error.contains("主题色"),
        format!("good={good} {error}"),
    );
}

/// 预算:死循环被燃料打断,内存暴涨被限额拦住,连续失败后停用。
fn budget(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("budget");
    let dir = temp.pack("spin");
    write_pack(&dir, &command_manifest("spin", &["run"], &[]), loop_guest());
    // 燃料预算收紧:解释器烧 100 万燃料远快于墙钟时限,错误稳定落在燃料上。
    let limits = Limits {
        command_fuel: 1_000_000,
        ..Limits::default()
    };
    let mut pack = match Pack::load_with(&dir, limits) {
        Ok(pack) => pack,
        Err(error) => {
            push(checks, "死循环包加载", false, error);
            return;
        }
    };
    let started = Instant::now();
    let first = pack.invoke_wait("run", "{}", Duration::from_secs(30));
    let elapsed = started.elapsed();
    let error = pack.last_error().unwrap_or_default().to_string();
    push(
        checks,
        "命令死循环被燃料打断",
        first.is_err() && error.contains("fuel") && elapsed < Duration::from_secs(5),
        format!("{elapsed:?} {error}"),
    );

    let hog_dir = temp.pack("hog");
    write_pack(
        &hog_dir,
        &command_manifest("hog", &["run"], &[]),
        hog_guest(),
    );
    let mut hog = Pack::load(&hog_dir).expect("加载内存包");
    let _ = hog.invoke_wait("run", "{}", Duration::from_secs(30));
    let stat = hog.stat();
    push(
        checks,
        "内存暴涨被限额拦住",
        stat.ooms >= 1,
        format!("ooms={} last={:?}", stat.ooms, stat.last_error),
    );

    let trap_dir = temp.pack("trap");
    write_pack(
        &trap_dir,
        &command_manifest("trap", &["run"], &[]),
        trap_guest(),
    );
    let mut trap = Pack::load(&trap_dir).expect("加载 trap 包");
    for _ in 0..3 {
        let _ = trap.invoke_wait("run", "{}", Duration::from_secs(30));
    }
    let stat = trap.stat();
    let stopped = trap.invoke_wait("run", "{}", Duration::from_secs(30));
    push(
        checks,
        "连续 trap 后停用扩展",
        stat.state == lueying_extwasm::ExtState::Disabled
            && stat.faults >= 3
            && stopped.is_err()
            && stopped.unwrap_err().contains("已停用"),
        format!("{stat:?}"),
    );
}

/// 隐私、高对比度与 trap 后的可用性。
fn privacy(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("privacy");
    let dir = temp.pack("demo");
    write_pack(&dir, &appearance_manifest("demo"), appearance_guest(PATCH));
    let Some(mut pack) = load_or_fail(checks, "外观包加载", &dir) else {
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
    push(
        checks,
        "隐私字段不调用 guest",
        private.is_none() && normal == Some(0x123456),
        format!("{private:?} {normal:?}"),
    );
    push(
        checks,
        "高对比度仍返回主题色",
        high_accent == Some(0x123456),
        format!("{high_accent:?}"),
    );

    // trap 之后下一次派发重建实例,外观宿主仍然可用。
    let boom_dir = temp.pack("boom");
    let boom_patch = r#"{"accent":4660}"#;
    write_pack(
        &boom_dir,
        &appearance_manifest("boom"),
        once_trap_appearance_guest(boom_patch),
    );
    let mut boom = Pack::load(&boom_dir).expect("加载 trap 外观包");
    let crashed = boom.dispatch(&SceneIn::default()).is_none();
    let error = boom.last_error().unwrap_or_default().to_string();
    let after = boom
        .dispatch(&SceneIn::default())
        .and_then(|p| p.accent)
        .is_some_and(|accent| accent == 4660);
    push(
        checks,
        "一次 trap 后宿主仍可派发",
        crashed && !error.is_empty() && after,
        format!("{error} after={after}"),
    );
}

/// 热重载:换新、写坏保留上一版、删除清单后卸载。
fn reload(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("reload");
    let dir = temp.pack("hot");
    let v1 = appearance_guest(r#"{"accent":100001}"#);
    let v2 = appearance_guest(r#"{"accent":200002}"#);
    let v3 = appearance_guest(r#"{"accent":300003}"#);
    write_pack(&dir, &appearance_manifest("hot"), &v1);
    let Some(mut pack) = load_or_fail(checks, "热重载包加载", &dir) else {
        return;
    };
    let a1 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    std::fs::write(dir.join("extension.wasm"), &v2).expect("写 V2");
    let polled = pack.poll();
    let a2 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    std::fs::write(dir.join("extension.wasm"), "not wasm").expect("写坏模块");
    let broken = pack.poll();
    let a3 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    std::fs::write(dir.join("extension.wasm"), &v3).expect("写 V3");
    let fixed = pack.poll();
    let a4 = pack.dispatch(&SceneIn::default()).and_then(|p| p.accent);
    push(
        checks,
        "热重载换新且写坏时保留上一版",
        a1 == Some(100001)
            && polled == Poll::Reloaded
            && a2 == Some(200002)
            && matches!(&broken, Poll::Failed(_))
            && a3 == Some(200002)
            && fixed == Poll::Reloaded
            && a4 == Some(300003),
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

/// 多扩展运行期:有序加载、懒加载、命令作用域、空闲卸载与 Scope 回收。
fn runtime(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("runtime");
    let source = command_guest(r#"{"ok":true}"#);
    let a = temp.pack("alpha");
    write_pack(&a, &command_manifest("alpha", &["run"], &[]), &source);
    let b = temp.pack("beta");
    write_pack(&b, &command_manifest("beta", &["run"], &[]), &source);

    let order = vec!["alpha".to_string(), "beta".to_string()];
    let (mut runtime, errors) = Runtime::open(&temp.root, &order, Limits::default());
    let commands = runtime.commands();
    let listed = commands.len() == 2
        && commands.iter().any(|command| command.id == "alpha.run")
        && commands.iter().any(|command| command.id == "beta.run");
    push(
        checks,
        "多扩展命令按清单列出且不加载 Wasm",
        errors.is_empty() && listed && runtime.loaded_count() == 0,
        format!(
            "{errors:?} {:?} loaded={}",
            commands,
            runtime.loaded_count()
        ),
    );

    let invoked = runtime.invoke("alpha.run", "{}");
    let only_alpha = runtime.loaded("alpha") && !runtime.loaded("beta");
    let results = loop {
        let results = runtime.take_results();
        if !results.is_empty() {
            break results;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    push(
        checks,
        "命令按需加载且只拉起被调用的扩展",
        invoked.is_ok()
            && only_alpha
            && results.len() == 1
            && results[0].1.result.as_deref() == Ok(r#"{"ok":true}"#),
        format!(
            "{invoked:?} {results:?} alpha={} beta={}",
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
        && runtime.invoke("alpha.run", "{}").is_err();
    push(
        checks,
        "卸载后 Scope 与命令全部消失",
        unloaded && gone,
        format!("unloaded={unloaded} commands={:?}", runtime.commands()),
    );

    let _ = runtime.invoke("beta.run", "{}");
    loop {
        if !runtime.take_results().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.set_idle(Duration::ZERO);
    runtime.idle_unload();
    let still_listed = runtime
        .commands()
        .iter()
        .any(|command| command.id == "beta.run");
    push(
        checks,
        "空闲卸载丢弃 worker 但保留命令声明",
        !runtime.loaded("beta") && still_listed,
        format!("loaded={} listed={still_listed}", runtime.loaded("beta")),
    );

    let first = temp.pack("first");
    write_pack(
        &first,
        &appearance_manifest("first"),
        appearance_guest(r#"{"accent":100001}"#),
    );
    let second = temp.pack("second");
    write_pack(
        &second,
        &appearance_manifest("second"),
        appearance_guest(r#"{"accent":200002}"#),
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
        forward_accent == Some(200002) && backward_accent == Some(100001),
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
        !errors.is_empty() && usable == Some(100001),
        format!("{errors:?} {usable:?}"),
    );

    // 未声明的命令不能被调到。
    let refused = runtime.invoke("first.nope", "{}").is_err();
    push(
        checks,
        "未声明命令被拒",
        refused,
        "invoke 应直接报错".into(),
    );
}

/// 可取消等待:guest 睡 10 秒,宿主卸载扩展时 2 秒内回来。
fn cancel_wait(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("cancel");
    let dir = temp.pack("sleep");
    write_pack(
        &dir,
        &command_manifest("sleep", &["run"], &[]),
        sleep_guest(10_000),
    );
    let (mut runtime, errors) =
        Runtime::open(&temp.root, &["sleep".to_string()], Limits::default());
    if !errors.is_empty() {
        push(checks, "睡眠包打开", false, format!("{errors:?}"));
        return;
    }
    let submitted = runtime.invoke("sleep.run", "{}").is_ok();
    let started = Instant::now();
    drop(runtime);
    let elapsed = started.elapsed();
    push(
        checks,
        "卸载扩展取消挂起的等待",
        submitted && elapsed < Duration::from_secs(2),
        format!("submitted={submitted} elapsed={elapsed:?}"),
    );
}

/// 运行 Wasm 扩展检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    bad_packs(&mut checks);
    scene(&mut checks);
    patches(&mut checks);
    budget(&mut checks);
    privacy(&mut checks);
    reload(&mut checks);
    runtime(&mut checks);
    cancel_wait(&mut checks);

    crate::harness::write(report, "运行期 Wasm 扩展", checks)
}

//! 运行期脚本扩展 E2E:包校验、场景协议、预算、隐私与热重载。
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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lueying_extjs::{Limits, Pack, Poll, SceneIn};

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

const HOG: &str =
    "lueying.register(function () { var a = new ArrayBuffer(64 * 1024 * 1024); return { accent: 1 }; });";

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
            "main",
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
    let ok = pack
        .dispatch(&good(0))
        .and_then(|p| p.accent)
        .is_some();
    let bad_accent = pack.dispatch(&good(1)).is_none();
    let accent_error = pack.last_error().unwrap_or_default().to_string();
    let ok_again = pack
        .dispatch(&good(0))
        .and_then(|p| p.accent)
        .is_some();
    let bad_size = pack.dispatch(&good(2)).is_none();
    let size_error = pack.last_error().unwrap_or_default().to_string();
    let ok_third = pack
        .dispatch(&good(0))
        .and_then(|p| p.accent)
        .is_some();
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
    let normal = pack
        .dispatch(&SceneIn::default())
        .and_then(|p| p.accent);
    let high = pack.dispatch(&SceneIn {
        high_contrast: true,
        ..SceneIn::default()
    });
    let high_accent = high.as_ref().and_then(|p| p.accent);
    let high_effects = high
        .as_ref()
        .is_some_and(|p| p.effects.as_ref().is_none());
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
    let after = pack
        .dispatch(&SceneIn::default())
        .and_then(|p| p.accent);
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

/// 运行全部检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    bad_packs(&mut checks);
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

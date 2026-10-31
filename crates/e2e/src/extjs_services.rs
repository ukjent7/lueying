//! 扩展服务缝与官方扩展 E2E:needs 门控、六个服务、热键绑定表与 shot-ocr。
//!
//! 先列失败方式:
//! 1. 未声明 needs 的服务调到了;缺提供者的服务静默成功;服务结果丢失。
//! 2. 绑定表解析、保留组合拒绝、when 过滤与表内冲突语义漂移。
//! 3. 官方 shot-ocr 清单或脚本与仓库脱节,截图→识别→上屏链路断掉。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lueying_extjs::bindings::{BindingTable, Input};
use lueying_extjs::{Done, Invoke, Limits, Provider, Registry, Runtime, Service, When};

use crate::extjs_harness::write_pack;
use crate::harness::{Temp, push};

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
    let gate_manifest = r#"{"id":"gate","name":"门","version":"1.0.0","api":2,"main":"index.js","needs":[],"activate":{"commands":[{"id":"try_ocr","title":"试"},{"id":"raw","title":"裸调"}]}}"#;
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
lueying.command("raw", function () {
  return __lueying_service_call("notify", "show", "{}", 77);
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

    // 引导层的 serviceFn 挡一次;直接调宿主全局入口也必须被 Rust 侧挡住。
    let raw = runtime.invoke("gate.raw", "");
    push(
        checks,
        "裸调服务入口也过 needs 门",
        matches!(&raw, Ok(Invoke::Done(text)) if text.contains("未在 needs 声明")),
        format!("{raw:?}"),
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
    let canonical = parse("alt+CTRL+o").map(|hotkey| hotkey.canonical()).ok();
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

/// 官方 shot-ocr 包:仓库里随安装包分发的同一份清单与脚本,配假提供者跑一遍。
/// 断言调用顺序、参数透传与黄金文本(OCR 结果两端空白由扩展自己裁掉)。
fn official_shot_ocr(checks: &mut Vec<(&'static str, bool, String)>) {
    const MANIFEST: &str = include_str!("../../../extensions/shot-ocr/manifest.json");
    const SCRIPT: &str = include_str!("../../../extensions/shot-ocr/index.js");
    const OCR_TEXT: &str = "  掠影黄金文本\n";

    /// 一个提供者接三路服务;按方法名分派,把调用顺序记下来。
    struct Chain {
        dir: PathBuf,
        calls: Mutex<Vec<String>>,
    }

    impl Provider for Chain {
        fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
            match method {
                "shot" => {
                    let file = self.dir.join("shot.bmp");
                    std::fs::write(&file, b"BMfake").map_err(|e| e.to_string())?;
                    self.calls.lock().unwrap().push("shot".into());
                    let payload = serde_json::json!({
                        "file": file.to_string_lossy(),
                        "width": 80,
                        "height": 20,
                    });
                    done.complete(Ok(payload.to_string()));
                }
                "recognize" => {
                    let parsed: serde_json::Value =
                        serde_json::from_str(args).map_err(|e| e.to_string())?;
                    self.calls.lock().unwrap().push(format!(
                        "recognize:{}",
                        parsed["file"].as_str().unwrap_or_default()
                    ));
                    let payload = serde_json::json!({ "text": OCR_TEXT, "lang": "fake" });
                    done.complete(Ok(payload.to_string()));
                }
                "deliver" => {
                    let parsed: serde_json::Value =
                        serde_json::from_str(args).map_err(|e| e.to_string())?;
                    self.calls.lock().unwrap().push(format!(
                        "deliver:{}",
                        parsed["text"].as_str().unwrap_or_default()
                    ));
                    done.complete(Ok(r#"{"delivered":true}"#.into()));
                }
                other => return Err(format!("假提供者不认识方法 {other}")),
            }
            Ok(())
        }
    }

    let temp = Temp::new("shot-ocr");
    let pack = temp.pack("shot-ocr");
    write_pack(&pack, MANIFEST, SCRIPT);
    let chain = Arc::new(Chain {
        dir: temp.root.clone(),
        calls: Mutex::new(Vec::new()),
    });
    let registry = Arc::new(Registry::new());
    registry.set(Service::Capture, chain.clone());
    registry.set(Service::Ocr, chain.clone());
    registry.set(Service::Output, chain.clone());
    let order = vec!["shot-ocr".to_string()];
    let (mut runtime, errors) = Runtime::open_with(&temp.root, &order, Limits::default(), registry);
    push(
        checks,
        "官方 shot-ocr 清单与脚本加载",
        errors.is_empty(),
        format!("{errors:?}"),
    );
    if !errors.is_empty() {
        return;
    }

    let started = runtime.invoke("shot-ocr.run", "");
    let mut settled = Vec::new();
    for _ in 0..10 {
        settled = runtime.pump(0);
        if !settled.is_empty() {
            break;
        }
    }
    let shot = temp.root.join("shot.bmp").to_string_lossy().into_owned();
    let expected = vec![
        "shot".to_string(),
        format!("recognize:{shot}"),
        "deliver:掠影黄金文本".to_string(),
    ];
    let calls = chain.calls.lock().unwrap().clone();
    push(
        checks,
        "官方 shot-ocr 截图→识别→上屏",
        matches!(started, Ok(Invoke::Pending(_)))
            && settled.len() == 1
            && settled[0].1.ok
            && settled[0].1.value.as_deref() == Some(r#"{"chars":6,"lang":"fake"}"#)
            && calls == expected,
        format!("{started:?} {settled:?} {calls:?}"),
    );
}

/// 运行服务缝与官方扩展检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    services(&mut checks);
    official_shot_ocr(&mut checks);
    hotkey_bindings(&mut checks);

    crate::harness::write(report, "扩展服务缝", checks)
}

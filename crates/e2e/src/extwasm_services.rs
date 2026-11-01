//! 扩展服务缝与官方扩展 E2E:needs 门控、缺提供者报错、绑定表与 shot-ocr。
//!
//! 先列失败方式:
//! 1. 未声明 needs 的服务在加载期就该被拒;声明了但没装提供者的服务静默成功。
//! 2. 绑定表解析、保留组合拒绝、when 过滤与表内冲突语义漂移。
//! 3. 官方 shot-ocr 清单或 guest 与仓库脱节,截图→识别→上屏链路断掉。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lueying_extwasm::bindings::{BindingTable, Input};
use lueying_extwasm::{Done, Limits, Provider, Registry, Runtime, Service, When};

use crate::extwasm_harness::{command_manifest, service_guest, write_pack};
use crate::harness::{Temp, push};

/// 服务缝:needs 门控与缺提供者的错误回传。
fn services(checks: &mut Vec<(&'static str, bool, String)>) {
    let temp = Temp::new("services");

    // guest 导入了 ocr 但清单没有声明:实例化直接失败。
    let gate_dir = temp.pack("gate");
    write_pack(
        &gate_dir,
        &command_manifest("gate", &["run"], &[]),
        &service_guest("ocr", "recognize", r#"{"file":"x"}"#),
    );
    let error = lueying_extwasm::Pack::load(&gate_dir)
        .err()
        .unwrap_or_default();
    push(
        checks,
        "未声明的服务在加载期被拒",
        error.contains("实例化失败") && error.contains("ocr"),
        error,
    );

    // 声明了 notify 但注册表没装提供者:命令结算为错误。
    let svc_dir = temp.pack("svc");
    write_pack(
        &svc_dir,
        &command_manifest("svc", &["run"], &["notify"]),
        &service_guest("notify", "show", "{}"),
    );
    let registry = Arc::new(Registry::new());
    let (mut runtime, errors) = Runtime::open_with(
        &temp.root,
        &["svc".to_string()],
        Limits::default(),
        registry,
    );
    if !errors.is_empty() {
        push(checks, "服务包打开", false, format!("{errors:?}"));
        return;
    }
    let _ = runtime.invoke("svc.run", "");
    let settled = loop {
        let results = runtime.take_results();
        if !results.is_empty() {
            break results;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let error = settled
        .iter()
        .find_map(|(_, finished)| finished.result.as_ref().err())
        .unwrap_or_default();
    push(
        checks,
        "缺提供者的服务报错",
        error.contains("没有提供者") && error.contains("notify"),
        error,
    );
}

/// 用户绑定表:解析归一、语法与保留组合拒绝、表内冲突、when 过滤。
fn hotkey_bindings(checks: &mut Vec<(&'static str, bool, String)>) {
    let parse = |combo: &str| lueying_extwasm::bindings::parse(combo);
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

/// 官方 shot-ocr 包:仓库里随安装包分发的同一份清单与 guest,配假提供者跑通。
/// 断言调用顺序、参数透传与结果(OCR 结果两端空白由扩展自己裁掉)。
fn official_shot_ocr(checks: &mut Vec<(&'static str, bool, String)>) {
    const MANIFEST: &str = include_str!("../../../extensions/shot-ocr/manifest.json");
    const OCR_TEXT: &str = "  掠影黄金文本\n";

    let wasm_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/shot-ocr/extension.wasm");
    let Ok(wasm) = std::fs::read(&wasm_path) else {
        push(
            checks,
            "shot-ocr guest 已构建",
            false,
            format!(
                "没有 {};先 cargo build -p shot-ocr --target wasm32-unknown-unknown --release",
                wasm_path.display()
            ),
        );
        return;
    };

    /// 一个提供者接三路服务;按方法名分派,把调用顺序记下来。
    struct Chain {
        dir: PathBuf,
        calls: Mutex<Vec<String>>,
    }

    impl Provider for Chain {
        fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
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
                "show" => done.complete(Ok("{}".into())),
                other => return Err(format!("假提供者不认识方法 {other}")),
            }
            Ok(())
        }
    }

    let temp = Temp::new("shot-ocr");
    let pack = temp.pack("shot-ocr");
    write_pack(&pack, MANIFEST, &wasm);
    let chain = Arc::new(Chain {
        dir: temp.root.to_path_buf(),
        calls: Mutex::new(Vec::new()),
    });
    let registry = Arc::new(Registry::new());
    registry.set(Service::Capture, chain.clone());
    registry.set(Service::Ocr, chain.clone());
    registry.set(Service::Output, chain.clone());
    registry.set(Service::Notify, Arc::new(NotifyStub));
    let order = vec!["shot-ocr".to_string()];
    let (mut runtime, errors) = Runtime::open_with(&temp.root, &order, Limits::default(), registry);
    push(
        checks,
        "官方 shot-ocr 清单与 guest 加载",
        errors.is_empty(),
        format!("{errors:?}"),
    );
    if !errors.is_empty() {
        return;
    }

    let submitted = runtime.invoke("shot-ocr.run", "").is_ok();
    let settled = loop {
        let results = runtime.take_results();
        if !results.is_empty() {
            break results;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let shot = temp.root.join("shot.bmp").to_string_lossy().into_owned();
    let expected = vec![
        "shot".to_string(),
        format!("recognize:{shot}"),
        "deliver:掠影黄金文本".to_string(),
    ];
    let calls = chain.calls.lock().unwrap().clone();
    let result: Option<serde_json::Value> = settled
        .first()
        .and_then(|(_, finished)| finished.result.as_ref().ok())
        .and_then(|json| serde_json::from_str(json).ok());
    push(
        checks,
        "官方 shot-ocr 截图→识别→上屏",
        submitted
            && settled.len() == 1
            && result.as_ref().is_some_and(|value| {
                value["chars"] == 6 && value["delivered"] == true && value["lang"] == "fake"
            })
            && calls == expected,
        format!("{settled:?} {calls:?}"),
    );
}

/// 占位通知提供者;链路里不该被调到。
struct NotifyStub;

impl Provider for NotifyStub {
    fn call(&self, _caller: &str, _method: &str, _args: &str, done: Done) -> Result<(), String> {
        done.complete(Ok("{}".into()));
        Ok(())
    }
}

/// 运行服务缝与官方扩展检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    services(&mut checks);
    official_shot_ocr(&mut checks);
    hotkey_bindings(&mut checks);

    crate::harness::write(report, "扩展服务缝", checks)
}

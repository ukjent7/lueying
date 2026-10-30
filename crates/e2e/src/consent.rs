//! 扩展授权 E2E:同意记录、首次安装拦截、升级重新同意与热重载门控。
//!
//! 失败方式:
//! 1. 未同意的包被加载,或同意过的包被误拦。
//! 2. 包内容变了但旧脚本继续热加载,绕过重新同意。
//! 3. 拒绝更新时旧版本被丢掉,扩展直接失效。
//! 4. 读屏 + 外发组合没有标记为敏感。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lueying_extjs::{ConsentGate, Invoke, Limits, Pack, Poll, Registry};
use lueying_extmeta::Manifest;
use lueying_extmeta::consent::ConsentBook;

const MANIFEST: &str = r#"{
  "id":"demo","name":"演示","version":"1.0.0","api":2,
  "activate":{"commands":[{"id":"run","title":"运行"}]},
  "needs":["capture"],
  "capabilities":["screen-capture"]
}"#;

const SCRIPT_V1: &str = "lueying.command('run', function () { return { v: 1 }; });";
const SCRIPT_V2: &str = "lueying.command('run', function () { return { v: 2 }; });";

struct Temp {
    root: PathBuf,
}

impl Temp {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "lueying-consent-e2e-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("建临时目录");
        Self { root }
    }

    fn pack(&self) -> PathBuf {
        let dir = self.root.join("demo");
        std::fs::create_dir_all(&dir).expect("建包目录");
        dir
    }

    fn consent(&self) -> PathBuf {
        self.root.join("consent.json")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn gate(path: &Path) -> ConsentGate {
    let path = path.to_path_buf();
    Arc::new(move |manifest: &Manifest, dir: &Path| {
        let book = ConsentBook::load(&path);
        let check = book
            .check(dir, manifest)
            .map_err(|error| format!("同意检查失败: {error}"))?;
        if check.required && !check.approved {
            return Err(format!("需在设置里同意: {}", check.items.join("、")));
        }
        Ok(())
    })
}

fn write_pack(dir: &Path, script: &str) {
    std::fs::write(dir.join("manifest.json"), MANIFEST).expect("写清单");
    std::fs::write(dir.join("index.js"), script).expect("写脚本");
}

fn approve(dir: &Path, consent: &Path) {
    let manifest = Manifest::parse(MANIFEST.as_bytes()).expect("清单应合法");
    let mut book = ConsentBook::load(consent);
    book.approve(dir, &manifest, 42).expect("同意");
    book.save(consent).expect("保存同意记录");
}

fn run_command(pack: &mut Pack) -> String {
    match pack.invoke("run", "{}") {
        Ok(Invoke::Done(json)) => json,
        other => format!("{other:?}"),
    }
}

pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    let temp = Temp::new();
    let dir = temp.pack();
    let consent = temp.consent();
    write_pack(&dir, SCRIPT_V1);
    let registry = Arc::new(Registry::new());

    let denied = Pack::load_with_gate(&dir, Limits::default(), &registry, Some(gate(&consent)));
    checks.push((
        "未同意不加载",
        denied
            .as_ref()
            .err()
            .is_some_and(|error| error.contains("需在设置里同意")),
        format!("{:?}", denied.err()),
    ));

    approve(&dir, &consent);
    let mut pack = match Pack::load_with_gate(&dir, Limits::default(), &registry, Some(gate(&consent)))
    {
        Ok(pack) => pack,
        Err(error) => {
            checks.push(("同意后加载", false, error));
            return write(report, &checks);
        }
    };
    let loaded = run_command(&mut pack);
    checks.push(("同意后加载", loaded == r#"{"v":1}"#, loaded));

    // 撤销同意并更新脚本:热重载必须拒绝,旧版本继续可用。
    ConsentBook::default().save(&consent).expect("清空同意");
    write_pack(&dir, SCRIPT_V2);
    let polled = pack.poll();
    checks.push((
        "更新需重新同意",
        matches!(&polled, Poll::Failed(error) if error.contains("更新未生效")),
        format!("{polled:?}"),
    ));
    let old = run_command(&mut pack);
    checks.push(("拒绝更新保留旧版本", old == r#"{"v":1}"#, old));

    approve(&dir, &consent);
    match Pack::load_with_gate(&dir, Limits::default(), &registry, Some(gate(&consent))) {
        Ok(mut updated) => {
            let version = run_command(&mut updated);
            checks.push(("重新同意后换新版本", version == r#"{"v":2}"#, version));
        }
        Err(error) => checks.push(("重新同意后换新版本", false, error)),
    }

    // 读屏 + 外发组合单独强提示。
    let risky = MANIFEST.replace(
        r#""capabilities":["screen-capture"]"#,
        r#""capabilities":["screen-capture","network"]"#,
    );
    let manifest = Manifest::parse(risky.as_bytes()).expect("清单应合法");
    let check = ConsentBook::default().check(&dir, &manifest);
    checks.push((
        "敏感组合标记",
        check.as_ref().is_ok_and(|check| check.sensitive),
        format!("{check:?}"),
    ));

    write(report, &checks)
}

fn write(report: &mut String, checks: &[(&'static str, bool, String)]) -> usize {
    report.push_str("\n## 扩展授权与审计\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0usize;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {} |\n",
            if *ok { "PASS" } else { "FAIL" },
            note.replace('|', "\\|").replace('\n', " ")
        ));
    }
    failures
}

//! Job Object 限额只在 Windows 上跑;CI 的 windows job 会执行本文件。

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::{Bypass, BypassConfig, ProcessLimiter};

fn echo_config() -> BypassConfig {
    BypassConfig {
        exe: crate::test_echo_exe(),
        args: Vec::new(),
        call_timeout: Duration::from_secs(5),
    }
}

#[test]
fn job_limiter_allows_healthy_call() {
    let limiter = crate::job::JobLimiter::new(Some(64 * 1024 * 1024), None);
    let bypass =
        Bypass::new(echo_config()).with_limiter(Arc::new(limiter) as Arc<dyn ProcessLimiter>);
    let answer = bypass
        .call("ping", r#"{"n":1}"#)
        .expect("限额下的调用应成功");
    let value: serde_json::Value = serde_json::from_str(&answer).expect("应答应是 JSON");
    assert_eq!(value["echo"]["n"], 1);
}

#[test]
fn job_limiter_failure_fails_call() {
    struct RejectLimiter;
    impl ProcessLimiter for RejectLimiter {
        fn attach(&self, _process: &std::process::Child) -> Result<(), String> {
            Err("测试注入的限额失败".to_string())
        }
    }
    let bypass =
        Bypass::new(echo_config()).with_limiter(Arc::new(RejectLimiter) as Arc<dyn ProcessLimiter>);
    let error = bypass.call("ping", "{}").expect_err("限额失败应上抛");
    assert!(error.contains("测试注入的限额失败"));
}

/// 直接对 cmd 子进程挂 Job,验证 attach 本身不报错、进程可正常退出。
#[test]
fn job_limiter_attaches_plain_child() {
    let mut child = Command::new("cmd")
        .args(["/c", "exit", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("cmd 应能启动");
    let limiter = crate::job::JobLimiter::new(None, Some(5_000));
    limiter.attach(&child).expect("挂接应成功");
    let status = child.wait().expect("进程应能回收");
    assert!(status.success());
}

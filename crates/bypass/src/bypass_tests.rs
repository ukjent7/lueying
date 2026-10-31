//! 旁路协议与提供者替换的平台无关测试;CI 的 engine job 在 Linux 上执行。

use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_extmeta::Service;
use lueying_extsvc::{Completion, Registry};

use crate::{Bypass, BypassConfig, BypassProvider, ProcessLimiter};

fn echo() -> Bypass {
    Bypass::new(BypassConfig {
        exe: crate::test_echo_exe(),
        args: Vec::new(),
        call_timeout: Duration::from_secs(5),
    })
}

/// 提供者在独立线程完成调用;轮询完成队列直到带该 token 的结果出现。
fn wait_completion(registry: &Registry, token: u64) -> Completion {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found = registry
            .take_completions()
            .into_iter()
            .find(|completion| completion.token == token);
        if let Some(completion) = found {
            return completion;
        }
        assert!(Instant::now() < deadline, "完成结果未到");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn protocol_echo_extracts_result() {
    let answer = echo()
        .call("recognize", r#"{"file":"C:/tmp/shot.bmp"}"#)
        .expect("回显应成功");
    let value: serde_json::Value = serde_json::from_str(&answer).expect("结果应是 JSON");
    assert_eq!(value["echo"]["file"], "C:/tmp/shot.bmp");
}

#[test]
fn invalid_args_are_rejected_locally() {
    let error = echo()
        .call("recognize", "不是 JSON")
        .expect_err("参数应被拒");
    assert!(error.contains("不是合法 JSON"));
}

#[test]
fn error_branch_surfaces_text() {
    let error = echo().call("boom", "{}").expect_err("error 应上抛");
    assert_eq!(error, "测试错误");
}

#[test]
fn timeout_kills_process() {
    let config = BypassConfig {
        exe: crate::test_echo_exe(),
        args: Vec::new(),
        call_timeout: Duration::from_millis(250),
    };
    let bypass = Bypass::new(config);
    let started = Instant::now();
    let error = bypass.call("hang", "{}").expect_err("挂起应超时");
    assert!(error.contains("超时"), "实际错误 {error}");
    // 杀完就返回,不等到替身自己的 30 秒睡眠结束。
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn limiter_failure_kills_process() {
    struct Reject;
    impl ProcessLimiter for Reject {
        fn attach(&self, _process: &std::process::Child) -> Result<(), String> {
            Err("注入失败".to_string())
        }
    }
    let bypass = echo().with_limiter(Arc::new(Reject) as Arc<dyn ProcessLimiter>);
    let error = bypass.call("ping", "{}").expect_err("限额失败应上抛");
    assert!(error.contains("注入失败"));
}

/// 服务提供者替换:同一个服务槽位装上旁路提供者后,注册表把调用
/// 转发到独立进程;隐私与审计仍由注册表层把关。
#[test]
fn provider_swap_routes_call_to_process() {
    let registry = Arc::new(Registry::new());
    registry.set(Service::Ocr, Arc::new(BypassProvider::new(echo())));
    assert!(registry.has(Service::Ocr));
    registry
        .call("ocr", "recognize", r#"{"file":"x.bmp"}"#, 7)
        .expect("调用应发出");
    let completion = wait_completion(&registry, 7);
    let result = completion.result.expect("旁路应答应成功");
    let value: serde_json::Value = serde_json::from_str(&result).expect("结果应是 JSON");
    assert_eq!(value["echo"]["file"], "x.bmp");
}

/// 替换语义:重装同一服务的提供者立即生效,旧进程句柄随之丢弃。
#[test]
fn provider_reinstall_overrides_slot() {
    let registry = Arc::new(Registry::new());
    registry.set(Service::Notify, Arc::new(BypassProvider::new(echo())));
    registry.set(Service::Notify, Arc::new(BypassProvider::new(echo())));
    assert!(registry.has(Service::Notify));
    registry
        .call("notify", "show", r#"{"title":"t","body":"b"}"#, 1)
        .expect("调用应发出");
    let completion = wait_completion(&registry, 1);
    // echo 替身把整个 args 回显在 result 里,证明调用确实走了旁路进程。
    let result = completion.result.expect("旁路应答应成功");
    assert!(result.contains(r#""body":"b""#));
}

/// 旁路冷启动测量:一次调用 = 拉起进程 + 往返 + 回收。
/// 原型的决策数据之一;数字随 `cargo test -p lueying-bypass -- --nocapture`
/// 出现在 CI 日志,常驻化决策(默认不常驻)以此为准。
#[test]
fn cold_start_measurement() {
    let started = Instant::now();
    for _ in 0..30 {
        let answer = echo()
            .call("recognize", r#"{"file":"m.bmp"}"#)
            .expect("测量调用应成功");
        assert!(answer.contains("m.bmp"));
    }
    let per_call = started.elapsed() / 30;
    println!("旁路冷启动均值(30 次): {per_call:?}");
    // 共享 runner 的调度抖动很大,只给上界,不当精确断言。
    assert!(per_call < Duration::from_secs(2), "冷启动均值 {per_call:?}");
}

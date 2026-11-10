//! 扩展宿主进程 E2E:服务端经行协议驱动独立的宿主进程。
//!
//! 宿主里的扩展死循环时,服务端的提交、上下文与候选快照调用都必须立刻返回,
//! 过期的补丁按超时丢弃,候选窗照常工作。宿主本身是本可执行文件以 `--ext-host` 运行的副本。

use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_ext::proto::SceneIn;
use lueying_ext::proto::candidate::CandidateState;
use lueying_ext::proto::link::Context;
use lueying_server::ext_link::{ExtLink, Handler, Replier, Request};

use lueying_ext::host::ConsentBook;

use crate::extwasm::harness::{
    ACCENT_A, ACCENT_B, command_manifest, fixture_a, fixture_b, raw_manifest, write_pack,
};
use crate::harness::{Temp, push};

/// 测试包的 id。
const PACK: &str = "loopdemo";
/// 一次服务端调用允许的最长耗时;远超它说明调用在等宿主。
const CALL_LIMIT: Duration = Duration::from_millis(50);

/// 跑宿主进程检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    let Some(fixture) = fixture_a() else {
        push(
            &mut checks,
            "扩展宿主进程 E2E 有组件测试客人",
            true,
            "缺 LUEYING_FIXTURE_WASM,跳过".into(),
        );
        return crate::harness::write(report, "扩展宿主进程", checks);
    };
    let temp = Temp::new("ext-host");
    let data = temp.root.join("data");
    let local = temp.root.join("local");
    let pack = data.join("extensions").join(PACK);
    let _ = std::fs::create_dir_all(&pack);
    write_pack(
        &pack,
        &command_manifest(PACK, &["run", "spin"], &[]),
        &fixture,
    );
    let handler: Handler = Arc::new(|_id, _request: Request, replier: Replier| {
        replier.send(Err("e2e 不处理服务请求".into()));
    });
    let exe = std::env::current_exe().unwrap_or_default();
    let args = vec![
        "--ext-host".to_string(),
        data.display().to_string(),
        local.display().to_string(),
    ];
    let link = ExtLink::spawn(exe, args, None, handler);
    link.set_config(vec![PACK.to_string()], Vec::new(), false, Vec::new());

    // 1. 宿主起来、加载包并上报标题。
    let deadline = Instant::now() + Duration::from_secs(20);
    let loaded = loop {
        if link
            .titles()
            .iter()
            .any(|(id, _)| id == &format!("{PACK}.run"))
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    push(
        &mut checks,
        "宿主进程加载包并上报命令标题",
        loaded,
        format!("titles={:?}", link.titles()),
    );

    // 2. 正常时补丁能回来,并且是当前序号。
    let seq = link.submit_scene(SceneIn::default());
    let patch = wait_patch(&link, Duration::from_secs(10), Some(seq));
    push(
        &mut checks,
        "正常外观补丁回到服务端",
        patch.as_ref().is_some_and(|(got, _)| *got == seq),
        format!("seq={seq} patch={patch:?}"),
    );

    // 3. 扩展死循环:宿主派发被卡住。
    link.invoke(&format!("{PACK}.spin"), "e2e");
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let seq = link.submit_scene(SceneIn::default());
    link.set_context(Context {
        private: false,
        editable: true,
        composing: true,
    });
    link.publish_candidates(CandidateState::default());
    let calls = started.elapsed();
    push(
        &mut checks,
        "宿主死循环时服务端调用立即返回",
        calls < CALL_LIMIT,
        format!("提交+上下文+候选 耗时 {calls:?}(上限 {CALL_LIMIT:?})"),
    );
    // 宿主忙时外观直接跳过(回空补丁),或干脆没有回应;两种都不能带主题色。
    let stuck = wait_patch(&link, PATCH_WAIT, None);
    let accent = stuck
        .as_ref()
        .and_then(|(_, patch)| patch.as_ref())
        .and_then(|patch| patch.accent);
    push(
        &mut checks,
        "宿主死循环时没有外观主题色回来,候选窗照常",
        accent.is_none(),
        format!("seq={seq} patch={stuck:?}"),
    );
    private_routing(&mut checks);
    crate::harness::write(report, "扩展宿主进程", checks)
}

/// 私密场景的路由(服务端过滤、宿主派发):不联网的扩展一直收到;联网扩展在用户确认
/// 私密使用之前收不到,确认之后才收到。外观主题色区分两个包:A 只有不联网的包、B 是联网包。
fn private_routing(checks: &mut Vec<(&'static str, bool, String)>) {
    let (Some(plain_wasm), Some(net_wasm)) = (fixture_a(), fixture_b()) else {
        push(
            checks,
            "私密路由有两个组件测试客人",
            true,
            "缺 LUEYING_FIXTURE_WASM / LUEYING_FIXTURE_B_WASM,跳过".into(),
        );
        return;
    };
    let temp = Temp::new("ext-private");
    let data = temp.root.join("data");
    let local = temp.root.join("local");
    let root = data.join("extensions");
    let plain = root.join("plain");
    let net = root.join("net");
    let _ = std::fs::create_dir_all(&plain);
    let _ = std::fs::create_dir_all(&net);
    write_pack(
        &plain,
        &raw_manifest("plain", r#""appearance":true"#),
        plain_wasm,
    );
    write_pack(
        &net,
        &raw_manifest(
            "net",
            r#""appearance":true,"needs":["network"],"network":{"allow":["api.example.com"]}"#,
        ),
        net_wasm,
    );
    let order = vec!["plain".to_string(), "net".to_string()];
    let Ok(net_manifest) = lueying_ext::host::manifest_at(&net) else {
        push(checks, "私密路由的联网包清单可读", false, String::new());
        return;
    };
    let mut book = ConsentBook::default();
    if book.approve(&net, &net_manifest, 1).is_err()
        || book.save(&root.join("consent.json")).is_err()
    {
        push(checks, "私密路由的同意记录写入", false, String::new());
        return;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let args = vec![
        "--ext-host".to_string(),
        data.display().to_string(),
        local.display().to_string(),
    ];
    let handler: Handler = Arc::new(|_id, _request: Request, replier: Replier| {
        replier.send(Err("e2e 不处理服务请求".into()));
    });
    let link = ExtLink::spawn(exe, args, None, handler);
    let recipients =
        |book: &ConsentBook| lueying_ext::host::private_recipients(&root, book, &order);
    link.set_config(order.clone(), Vec::new(), false, recipients(&book));
    let private_scene = SceneIn {
        private: true,
        ..SceneIn::default()
    };
    let normal = scene_accent(&link, SceneIn::default());
    let private = scene_accent(&link, private_scene.clone());

    if book.set_private(&net, &net_manifest, true).is_err()
        || book.save(&root.join("consent.json")).is_err()
    {
        push(checks, "私密确认写入", false, String::new());
        return;
    }
    link.set_config(order.clone(), Vec::new(), false, recipients(&book));
    let confirmed = scene_accent(&link, private_scene);

    push(
        checks,
        "普通场景两个外观都收到",
        normal == Some(ACCENT_B),
        format!("{normal:?}"),
    );
    push(
        checks,
        "私密场景到达不联网扩展",
        private == Some(ACCENT_A),
        format!("{private:?}"),
    );
    push(
        checks,
        "私密场景不到达未确认私密使用的联网扩展",
        private != Some(ACCENT_B),
        format!("{private:?}"),
    );
    push(
        checks,
        "确认私密使用后私密场景到达联网扩展",
        confirmed == Some(ACCENT_B),
        format!("{confirmed:?}"),
    );
}

/// 提交一帧场景并等它的补丁,返回主题色;补丁超过新鲜期会被丢弃,所以超时后重交。
fn scene_accent(link: &ExtLink, scene: SceneIn) -> Option<u32> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let seq = link.submit_scene(scene.clone());
        if let Some((_, patch)) = wait_patch(link, Duration::from_millis(900), Some(seq)) {
            return patch.and_then(|patch| patch.accent);
        }
    }
    None
}

/// 等待宿主的补丁,超时返回 `None`;`want` 给出时只接受该序号的补丁。
fn wait_patch(
    link: &ExtLink,
    within: Duration,
    want: Option<u64>,
) -> Option<(u64, Option<lueying_ext::proto::ScenePatch>)> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(patch) = link.take_patch() {
            // 宿主换配置时会先发一条序号更早的清空补丁,只认目标序号。
            if want.is_none_or(|seq| seq == patch.0) {
                return Some(patch);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 死循环检查里等补丁的时长:远超补丁的有效期。
const PATCH_WAIT: Duration = Duration::from_millis(1500);

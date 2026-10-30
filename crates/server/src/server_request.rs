//! 管道请求分发。

use lueying_protocol::EngineStatus;
use lueying_render::effect::{self, Slot};

use super::*;

/// 三个槽位的模板源码;设置窗写进效果目录当起点。
fn effect_templates() -> Vec<(String, String)> {
    Slot::ALL
        .iter()
        .map(|slot| {
            (
                slot.file_name().to_string(),
                effect::template(*slot).to_string(),
            )
        })
        .collect()
}

pub(crate) fn dispatch(state: &State, req: Request) -> Reply {
    // 任何请求后候选窗都可能要重摆,先记账再处理;只读上报不触发重绘。
    if !matches!(&req, Request::Sync { .. } | Request::Context { .. }) {
        state.version.fetch_add(1, Ordering::Relaxed);
    }
    let mut sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    match req {
        Request::Hello { session, host } => {
            println!("会话 {session} 上线({host})");
            // 同一线程号重新上线(Server 重启、DLL 重连)必须从头开始,
            // 不能继承上一个线程留下的组合与待办。
            sessions.remove(&session);
            crate::settle::cancel(state, session);
            // 按应用覆盖:apps.ini 里写了的键盖过全局设置。
            let app = config::app_overrides(&host);
            let mut engine = {
                let mut cfg = state
                    .config
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .engine();
                if let Some(punct) = app.punct {
                    cfg.chinese_punct = punct;
                }
                let scheme = {
                    let guard = state.scheme.read().unwrap_or_else(|p| p.into_inner());
                    Arc::clone(&*guard)
                };
                Engine::new(Arc::clone(&state.dict), cfg)
                    .with_user(Arc::clone(&state.user))
                    .with_modules(Arc::clone(&state.modules))
                    .with_scheme(scheme)
            };
            if let Some(g) = &state.gram {
                engine = engine.with_gram(Arc::clone(g));
            }
            engine.set_mode(current_mode(state));
            engine.set_fullwidth(state.fullwidth.load(Ordering::Relaxed));
            engine.set_now(local_time());
            engine.set_host(&host);
            if app.learning == Some(false) {
                engine.set_learning(false);
            }
            let chinese = engine.mode() == Mode::Chinese;
            sessions.insert(
                session,
                Session {
                    engine,
                    view: SessionView {
                        host,
                        chinese,
                        fullwidth: state.fullwidth.load(Ordering::Relaxed),
                        ..SessionView::default()
                    },
                    pending: None,
                    ui_reply: None,
                    sync_rerank: state.sync_model.as_ref().map(Rerank::new),
                    settled_rerank: state
                        .settled_model
                        .as_ref()
                        .map(|m| Arc::new(Mutex::new(Rerank::new(m)))),
                    host_draws: false,
                    private: false,
                    context_epoch: 0,
                    context_seq: 0,
                },
            );
            state.active.store(session, Ordering::Relaxed);
            neutral(state)
        }
        Request::Key { session, key } => {
            state.active.store(session, Ordering::Relaxed);
            let Some(s) = sessions.get_mut(&session) else {
                // 会话已经下线(Server 刚重启/宿主走了 Goodbye):让键直通,
                // 不能返回一个「已消费」的空应答把按键吞掉。
                return Reply {
                    passthrough: true,
                    chinese: state.chinese.load(Ordering::Relaxed),
                    fullwidth: state.fullwidth.load(Ordering::Relaxed),
                    ..Reply::default()
                };
            };
            // 鼠标点选可能还没被 TSF 的 Sync 取走;先记下,和本次按键的
            // 结果一起写,避免快速连打把点选上屏吞掉。
            let carried = s.ui_reply.take();
            let engine_key = engine_key(key);
            let shift_toggle = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .shift_toggle;
            if engine_key == Key::Shift && !shift_toggle {
                // 不切模式,但要把当前内联文本原样带回,免得 TSF 清掉组合。
                let mut reply = current_reply(s);
                if let Some(carried) = carried {
                    let mut commits = carried.commits;
                    commits.extend(reply.commits);
                    reply.commits = commits;
                }
                return reply;
            }
            let before_mode = s.engine.mode();
            let before_rev = s.engine.revision();
            // 每次按键刷新本地时间,跨午夜/夏令时切换后日期仍然正确。
            s.engine.set_now(local_time());
            let events = s.engine.process(engine_key);
            // 触发词整串匹配时把动作交给引擎注入候选;命令是否可执行看标题表。
            sync_action(state, s);
            // 只有候选集合重算过才重排;翻页/移动高亮不能把用户的选择重置。
            if s.engine.revision() != before_rev {
                apply_sync_rerank(s);
            }
            // 选中动作候选:执行扩展命令,不上屏文字。
            for event in &events {
                if let Event::Action(id) = event {
                    invoke_extension(state, id);
                }
            }
            let mut reply = reply_from(s, &events);
            if let Some(carried) = carried {
                if reply.passthrough {
                    // 直通键不经过编辑会话,点选结果留给下一次 Sync。
                    s.ui_reply = Some(carried);
                } else {
                    let mut commits = carried.commits;
                    commits.extend(reply.commits);
                    reply.commits = commits;
                }
            }
            refresh_view(s);
            fill_view(&mut reply, s);
            s.pending = Some(reply.clone());
            crate::settle::schedule(state, session, s);
            let mode_changed = s.engine.mode() != before_mode;
            let chinese = s.engine.mode() == Mode::Chinese;
            if mode_changed {
                set_global_mode(state, &mut sessions, chinese);
            }
            reply
        }
        Request::ToggleMode { session } => {
            state.active.store(session, Ordering::Relaxed);
            let Some(s) = sessions.get_mut(&session) else {
                return neutral(state);
            };
            let carried = s.ui_reply.take();
            let before_rev = s.engine.revision();
            let events = s.engine.process(Key::Shift);
            // 组合被落定时动作也随之撤销,避免下一次组合用旧触发词注入。
            sync_action(state, s);
            if s.engine.revision() != before_rev {
                apply_sync_rerank(s);
            }
            let mut reply = reply_from(s, &events);
            if let Some(carried) = carried {
                let mut commits = carried.commits;
                commits.extend(reply.commits);
                reply.commits = commits;
            }
            // 模式切换可能已把组合落定,之前缓存的按键应答不能再写回。
            s.pending = None;
            refresh_view(s);
            fill_view(&mut reply, s);
            crate::settle::schedule(state, session, s);
            let chinese = s.engine.mode() == Mode::Chinese;
            set_global_mode(state, &mut sessions, chinese);
            reply
        }
        Request::Sync { session } => {
            let Some(s) = sessions.get_mut(&session) else {
                return neutral(state);
            };
            let mut reply = s.ui_reply.take().unwrap_or_default();
            reply.preedit = s.view.preedit.clone();
            reply.chinese = state.chinese.load(Ordering::Relaxed);
            fill_view(&mut reply, s);
            reply
        }
        Request::Context {
            session,
            text,
            private,
            fallback,
            hwnd,
            epoch,
        } => {
            let mut want_uia = false;
            let mut uia_context = String::new();
            let mut uia_seq = 0;
            if let Some(s) = sessions.get_mut(&session) {
                // 输入范围每次焦点/组合开始都重报;私密字段不学、不记前文。
                let private_changed = s.private != private;
                s.engine.set_learning(!private);
                s.private = private;
                if private_changed {
                    // 隐私标记变了要重摆一帧:扩展层在密码框里整个停用。
                    state.version.fetch_add(1, Ordering::Relaxed);
                }
                s.context_epoch = epoch;
                s.context_seq = s.context_seq.wrapping_add(1);
                uia_seq = s.context_seq;
                if private {
                    s.engine.clear_context();
                } else {
                    apply_context(state, session, s, &text);
                }
                want_uia = !private && fallback && hwnd != 0;
                if want_uia {
                    // UIA 回来时若这里已经因为上屏而变化,就丢弃旧文本。
                    uia_context = s.engine.context().to_string();
                }
            }
            if private {
                crate::settle::cancel(state, session);
            } else if want_uia && let Some(tx) = state.uia_tx.as_ref() {
                crate::uia::say(&format!("UIA: 回退受理 hwnd={hwnd}"));
                // 空上下文已经先应用;UIA 结果回来再按代次/页/高亮决定
                // 是否采用。容量 1,前一次查询没回来时直接丢新任务。
                let _ = tx.try_send(crate::uia::Job {
                    session,
                    hwnd,
                    epoch,
                    seq: uia_seq,
                    context: uia_context,
                });
            }
            Reply::default()
        }
        Request::TakeKey { session } => sessions
            .get_mut(&session)
            .and_then(|s| s.pending.take())
            .unwrap_or_else(|| neutral(state)),
        Request::SetMode { session, chinese } => {
            state.active.store(session, Ordering::Relaxed);
            let reply = if let Some(s) = sessions.get_mut(&session) {
                let carried = s.ui_reply.take();
                let before_rev = s.engine.revision();
                // 组合中切模式要先把拼音落定,不能直接清掉用户敲了一半的内容。
                let events = switch_mode(s, chinese);
                if s.engine.revision() != before_rev {
                    apply_sync_rerank(s);
                }
                let mut reply = reply_from(s, &events);
                if let Some(carried) = carried {
                    let mut commits = carried.commits;
                    commits.extend(reply.commits);
                    reply.commits = commits;
                }
                // 切模式前的按键应答已过期,避免 OnKeyDown 把旧内联文本写回。
                s.pending = None;
                refresh_view(s);
                fill_view(&mut reply, s);
                crate::settle::schedule(state, session, s);
                reply
            } else {
                neutral(state)
            };
            set_global_mode(state, &mut sessions, chinese);
            reply
        }
        Request::ToggleFullwidth { session } => {
            state.active.store(session, Ordering::Relaxed);
            if sessions.contains_key(&session) {
                let fullwidth = !state.fullwidth.load(Ordering::Relaxed);
                set_global_fullwidth(state, &mut sessions, fullwidth);
            }
            neutral(state)
        }
        Request::SetFullwidth { session, fullwidth } => {
            state.active.store(session, Ordering::Relaxed);
            if !sessions.contains_key(&session) {
                return neutral(state);
            }
            set_global_fullwidth(state, &mut sessions, fullwidth);
            // 系统 compartment 的回调可能正组着句;带上当前内联文本,
            // 让 TSF 的 apply_reply 重写同一份预编辑而不是把它清掉。
            let mut reply = neutral(state);
            if let Some(s) = sessions.get(&session) {
                reply.preedit = s.view.preedit.clone();
                fill_view(&mut reply, s);
            }
            reply
        }
        Request::Focus { session, focused } => {
            // 失焦通知可能晚于新窗口的聚焦通知;只允许聚焦者抢占 active,
            // 失焦只清理自己,避免候选窗跟到已经切走的旧输入框。
            if focused {
                state.active.store(session, Ordering::Relaxed);
            } else if state.active.load(Ordering::Relaxed) == session {
                state.active.store(0, Ordering::Relaxed);
            }
            let mut reply = neutral(state);
            if let Some(s) = sessions.get_mut(&session) {
                // 失焦/换输入框时作废在途的 UIA 读取;新 Context 会带来新代次。
                // 隐私标记属于上一个输入范围,新 Context 上报前先清掉。
                s.private = false;
                s.context_epoch = s.context_epoch.wrapping_add(1);
                s.context_seq = s.context_seq.wrapping_add(1);
                s.pending = None;
                let carried = s.ui_reply.take();
                if !focused {
                    // 失焦:把敲了一半的拼音整串落定,再清上下文。
                    // 只按一次空格会留下吃前缀候选之后的残余拼音。
                    let events = s.engine.flush();
                    reply = reply_from(s, &events);
                    if let Some(carried) = carried {
                        let mut commits = carried.commits;
                        commits.extend(reply.commits);
                        reply.commits = commits;
                    }
                    s.engine.clear_context();
                    crate::settle::cancel(state, session);
                } else {
                    // 回到本线程/新输入框:若上一次失焦通知丢了,残留组合
                    // 不能带进新上下文。正常失焦已由上面的 false 分支落定。
                    if s.engine.composition().is_some() {
                        let _ = s.engine.process(Key::Esc);
                    }
                    // 点击候选后应用可能强行结束组句;保留待写文本,
                    // 下一次按键或 Sync 再补进文档。
                    s.ui_reply = carried;
                    s.engine.clear_context();
                }
                refresh_view(s);
                fill_view(&mut reply, s);
                // 清组合/换输入框后,待写回的内联文本必须跟上当前状态。
                sync_ui_reply(s);
            }
            reply
        }
        Request::CompositionTerminated { session } => {
            if let Some(s) = sessions.get_mut(&session) {
                let _ = s.engine.process(Key::Esc);
                // 已点选、还没写回的文本不能丢;下次按键/Sync 继续带上。
                crate::settle::cancel(state, session);
                refresh_view(s);
                sync_ui_reply(s);
            }
            neutral(state)
        }
        Request::Caret {
            session,
            left,
            top,
            right,
            bottom,
        } => {
            state.active.store(session, Ordering::Relaxed);
            if let Some(s) = sessions.get_mut(&session) {
                s.view.caret = Some((left, top, right, bottom));
                // 组合已经建立、上下文也可能刚上报;在这里排停顿任务最可靠。
                crate::settle::schedule(state, session, s);
            }
            Reply::default()
        }
        Request::Pick { session, slot } => {
            state.active.store(session, Ordering::Relaxed);
            if let Some(s) = sessions.get_mut(&session) {
                let before = s.engine.revision();
                let events = s.engine.pick_in_page(slot);
                for event in &events {
                    if let Event::Action(id) = event {
                        invoke_extension(state, id);
                    }
                }
                if s.engine.revision() != before {
                    apply_sync_rerank(s);
                }
                refresh_view(s);
                let commits: Vec<String> = events
                    .iter()
                    .filter_map(|e| match e {
                        Event::Commit(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect();
                if !commits.is_empty() {
                    let chinese = s.view.chinese;
                    let fullwidth = s.view.fullwidth;
                    let pending = s.ui_reply.get_or_insert_with(|| Reply {
                        chinese,
                        fullwidth,
                        ..Reply::default()
                    });
                    pending.commits.extend(commits);
                    pending.preedit = s.view.preedit.clone();
                    pending.chinese = chinese;
                    pending.fullwidth = fullwidth;
                }
                crate::settle::schedule(state, session, s);
            }
            Reply::default()
        }
        Request::Pin { session, slot } => {
            state.active.store(session, Ordering::Relaxed);
            if let Some(s) = sessions.get_mut(&session) {
                s.engine.pin_in_page(slot);
                refresh_view(s);
                crate::settle::schedule(state, session, s);
            }
            Reply::default()
        }
        Request::Demote { session, slot } => {
            state.active.store(session, Ordering::Relaxed);
            if let Some(s) = sessions.get_mut(&session) {
                s.engine.demote_in_page(slot);
                refresh_view(s);
                crate::settle::schedule(state, session, s);
            }
            Reply::default()
        }
        Request::PickIndex { session, index } => {
            state.active.store(session, Ordering::Relaxed);
            let mut reply = Reply::default();
            if let Some(s) = sessions.get_mut(&session) {
                let before = s.engine.revision();
                let events = s.engine.pick(index);
                for event in &events {
                    if let Event::Action(id) = event {
                        invoke_extension(state, id);
                    }
                }
                if s.engine.revision() != before {
                    apply_sync_rerank(s);
                }
                // 宿主自绘候选时由 TSF 直接收走上屏文本并写进文档,
                // 不走候选窗的 Sync 队列。
                reply = reply_from(s, &events);
                refresh_view(s);
                fill_view(&mut reply, s);
                crate::settle::schedule(state, session, s);
            }
            reply
        }
        Request::Finalize { session } => {
            state.active.store(session, Ordering::Relaxed);
            let Some(s) = sessions.get_mut(&session) else {
                return neutral(state);
            };
            let carried = s.ui_reply.take();
            let mut reply = Reply {
                chinese: s.engine.mode() == Mode::Chinese,
                ..Reply::default()
            };
            if let Some(text) = s.engine.finalize_display() {
                reply.commits.push(text);
            }
            if let Some(carried) = carried {
                let mut commits = carried.commits;
                commits.extend(reply.commits);
                reply.commits = commits;
            }
            // 组合已经结束,之前的按键应答不能再写回内联拼音。
            s.pending = None;
            refresh_view(s);
            fill_view(&mut reply, s);
            crate::settle::schedule(state, session, s);
            reply
        }
        Request::SelectIndex { session, index } => {
            state.active.store(session, Ordering::Relaxed);
            let mut reply = Reply::default();
            if let Some(s) = sessions.get_mut(&session) {
                if s.engine.set_highlight(index) {
                    // 用户正在挑候选,停顿重排不能把高亮挪走。
                    crate::settle::cancel(state, session);
                    refresh_view(s);
                    sync_ui_reply(s);
                }
                fill_view(&mut reply, s);
                // 宿主自绘候选时,高亮变化要同步到内联预编辑;否则文档里
                // 还是旧候选,点选前后看到的文本对不上。
                reply.preedit = s
                    .engine
                    .composition()
                    .map(|c| c.display.clone())
                    .unwrap_or_default();
                reply.chinese = s.engine.mode() == Mode::Chinese;
            }
            reply
        }
        Request::Page { session, delta } => {
            state.active.store(session, Ordering::Relaxed);
            if let Some(s) = sessions.get_mut(&session)
                && s.engine.page_by(delta)
            {
                // 用户正在翻看候选,别让停顿重排回来把当前页换掉。
                crate::settle::cancel(state, session);
                refresh_view(s);
                sync_ui_reply(s);
            }
            Reply::default()
        }
        Request::UiMode {
            session,
            host_draws,
        } => {
            if let Some(s) = sessions.get_mut(&session) {
                s.host_draws = host_draws;
            }
            neutral(state)
        }
        Request::ReloadConfig => {
            if let Some(path) = &state.config_path {
                let cfg = config::load(path);
                apply_config(state, &mut sessions, cfg);
            }
            Reply::default()
        }
        Request::TogglePunct => {
            let cfg = {
                let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
                cfg.chinese_punct = !cfg.chinese_punct;
                cfg.clone()
            };
            // Ctrl+. 是隐藏快捷键;借模式徽标短暂显示「，。」或「,.」,让用户知道切换结果。
            let label = if cfg.chinese_punct { "，。" } else { ",." };
            if let Some(path) = &state.config_path
                && let Err(e) = config::save(path, &cfg)
            {
                println!("保存配置 {} 失败: {e}", path.display());
            }
            apply_config(state, &mut sessions, cfg);
            show_hud(state, &sessions, label, None);
            neutral(state)
        }
        Request::ExportUser => Reply {
            user: state
                .user
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .export(),
            ..Reply::default()
        },
        Request::ImportUser { text } => {
            state
                .user
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .import(&text);
            if let Some(path) = crate::user_store::path() {
                crate::user_store::flush(&state.user, &path);
            }
            let cfg = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            apply_config(state, &mut sessions, cfg);
            group_reply(&state.user)
        }
        Request::ClearUser => {
            state
                .user
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            // 立即落盘:清空后马上退出也不能让旧词库在下一次启动回来。
            if let Some(path) = crate::user_store::path() {
                crate::user_store::flush(&state.user, &path);
            }
            // 正在组句的候选可能含刚学过的词;按当前配置重算一遍。
            let cfg = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            apply_config(state, &mut sessions, cfg);
            Reply::default()
        }
        Request::FuzzyStats => group_reply(&state.user),
        Request::EffectTemplates => Reply {
            templates: effect_templates(),
            ..Reply::default()
        },
        Request::Status => Reply {
            status: Some(EngineStatus {
                dict_words: state.dict.len(),
                dict_source: state.dict_source.to_string(),
                gram: state.gram.is_some(),
                rerank4m: state.sync_model.is_some(),
                rerank25m: state.settled_model.is_some(),
                server_version: env!("CARGO_PKG_VERSION").to_string(),
            }),
            ..Reply::default()
        },
        Request::ResetFuzzy => {
            state
                .user
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .reset_groups();
            if let Some(path) = crate::user_store::path() {
                crate::user_store::flush(&state.user, &path);
            }
            let cfg = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            apply_config(state, &mut sessions, cfg);
            group_reply(&state.user)
        }
        Request::Shutdown => {
            // 先让管道把应答写完,再由本进程退出;退出前同步落盘。
            if let Some(path) = crate::user_store::path() {
                crate::user_store::flush(&state.user, &path);
            }
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(0);
            });
            Reply::default()
        }
        Request::Goodbye { session } => {
            if sessions.remove(&session).is_some() {
                println!("会话 {session} 下线");
            }
            if state.active.load(Ordering::Relaxed) == session {
                state.active.store(0, Ordering::Relaxed);
            }
            crate::settle::cancel(state, session);
            Reply::default()
        }
    }
}

/// 当前组合的整串输入命中用户触发表时,把动作交给引擎注入候选。
/// 标题表由脚本线程维护:命令不存在(扩展没启用或已卸载)就不出动作候选。
fn sync_action(state: &State, s: &mut Session) {
    let action = s.engine.composition().and_then(|comp| {
        let raw = comp.raw.replace('\'', "");
        let command = state
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .triggers
            .iter()
            .find(|(trigger, _)| trigger == &raw)
            .map(|(_, command)| command.clone())?;
        let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
        let hub = hub.as_ref()?;
        let titles = hub.titles.lock().unwrap_or_else(|p| p.into_inner());
        let title = titles.get(&command)?.clone();
        Some(Action {
            trigger: raw,
            id: command,
            title,
        })
    });
    s.engine.set_action(action);
}

/// 把动作候选选中的命令交给脚本线程执行;扩展不在线时只写日志。
fn invoke_extension(state: &State, id: &str) {
    let hub = state.ext_hub.lock().unwrap_or_else(|p| p.into_inner());
    match hub.as_ref() {
        Some(hub) => {
            let _ = hub.invoke.send(id.to_string());
        }
        None => println!("动作候选跳过:脚本线程未启动({id})"),
    }
}

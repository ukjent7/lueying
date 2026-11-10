//! 管道请求分发。
//!
//! 会话表锁只用来查句柄;引擎状态由每个会话自己的锁护住,一次按键不会
//! 把别的窗口排在后面。改动结束把视图发布出去并叫醒候选窗。

use lueying_protocol::{EngineStatus, RerankLatency};

use super::*;

use super::ext::{
    command_run, commands_list, invoke_extension, settings_get, settings_set, sync_action,
};

pub(crate) fn dispatch(state: &State, req: Request) -> Reply {
    match req {
        Request::Hello { session, host } => {
            println!("会话 {session} 上线({host})");
            // 同一线程号重新上线(Server 重启、DLL 重连)必须从头开始,
            // 不能继承上一个线程留下的组合与待办。
            let _ = state
                .sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&session);
            crate::settle::cancel(state, session);
            // 按应用覆盖:apps.toml 已经并进配置层,这里只查表。
            let app = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .app_override(&host);
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
            let handle = SessionHandle::new(Session {
                engine,
                view: SessionView {
                    host,
                    chinese,
                    fullwidth: state.fullwidth.load(Ordering::Relaxed),
                    ..SessionView::default()
                },
                pending: None,
                ui_reply: None,
                sync_rerank: state.sync_model.as_ref().map(|model| {
                    Arc::new(Mutex::new(Rerank::with_metrics(
                        model,
                        Arc::clone(&state.rerank_sync_metrics),
                    )))
                }),
                settled_rerank: state
                    .settled_model
                    .as_ref()
                    .or(state.sync_model.as_ref())
                    .map(|model| {
                        Arc::new(Mutex::new(Rerank::with_metrics(
                            model,
                            Arc::clone(&state.rerank_settle_metrics),
                        )))
                    }),
                host_draws: false,
                private: false,
                context_epoch: 0,
                context_seq: 0,
                settle_pick: None,
            });
            state
                .sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(session, handle);
            state.activate(session);
            touch_view(state);
            neutral(state)
        }
        Request::Key { session, key } => {
            state.activate(session);
            let Some(handle) = state.session(session) else {
                // 会话已经下线(Server 刚重启/宿主走了 Goodbye):让键直通,
                // 不能返回一个「已消费」的空应答把按键吞掉。
                return Reply {
                    passthrough: true,
                    chinese: state.chinese.load(Ordering::Relaxed),
                    fullwidth: state.fullwidth.load(Ordering::Relaxed),
                    ..Reply::default()
                };
            };
            let mut switched = None;
            let reply = handle.with(state, |s| {
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
                    crate::sync_rerank::schedule(state, session, s);
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
                fill_view(&mut reply, &s.view);
                s.pending = Some(reply.clone());
                crate::settle::schedule(state, session, s);
                if s.engine.mode() != before_mode {
                    switched = Some(s.engine.mode() == Mode::Chinese);
                }
                reply
            });
            if let Some(chinese) = switched {
                set_global_mode(state, chinese);
            }
            reply
        }
        Request::ToggleMode { session } => {
            state.activate(session);
            let Some(handle) = state.session(session) else {
                return neutral(state);
            };
            let reply = handle.with(state, |s| {
                let carried = s.ui_reply.take();
                let before_rev = s.engine.revision();
                let events = s.engine.process(Key::Shift);
                // 组合被落定时动作也随之撤销,避免下一次组合用旧触发词注入。
                sync_action(state, s);
                if s.engine.revision() != before_rev {
                    crate::sync_rerank::schedule(state, session, s);
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
                fill_view(&mut reply, &s.view);
                crate::settle::schedule(state, session, s);
                reply
            });
            set_global_mode(state, reply.chinese);
            reply
        }
        Request::Sync { session } => {
            let Some(reply) = state.with_session(session, |s| {
                crate::settle::take_pick(s);
                let mut reply = s.ui_reply.take().unwrap_or_default();
                reply.preedit = s.view.preedit.clone();
                reply.chinese = state.chinese.load(Ordering::Relaxed);
                fill_view(&mut reply, &s.view);
                reply
            }) else {
                return neutral(state);
            };
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
            let _ = state.with_session(session, |s| {
                // 输入范围每次焦点/组合开始都重报;私密字段不学、不记前文。
                s.engine.set_learning(!private);
                s.private = private;
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
            });
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
        Request::TakeKey { session } => state
            .with_session(session, |s| s.pending.take())
            .flatten()
            .unwrap_or_else(|| neutral(state)),
        Request::SetMode { session, chinese } => {
            state.activate(session);
            let Some(handle) = state.session(session) else {
                return neutral(state);
            };
            let reply = handle.with(state, |s| {
                let carried = s.ui_reply.take();
                let before_rev = s.engine.revision();
                // 组合中切模式要先把拼音落定,不能直接清掉用户敲了一半的内容。
                let events = switch_mode(s, chinese);
                if s.engine.revision() != before_rev {
                    crate::sync_rerank::schedule(state, session, s);
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
                fill_view(&mut reply, &s.view);
                crate::settle::schedule(state, session, s);
                reply
            });
            set_global_mode(state, chinese);
            reply
        }
        Request::ToggleFullwidth { session } => {
            state.activate(session);
            if state.session(session).is_some() {
                let fullwidth = !state.fullwidth.load(Ordering::Relaxed);
                set_global_fullwidth(state, fullwidth);
            }
            neutral(state)
        }
        Request::SetFullwidth { session, fullwidth } => {
            state.activate(session);
            if state.session(session).is_none() {
                return neutral(state);
            }
            set_global_fullwidth(state, fullwidth);
            // 系统 compartment 的回调可能正组着句;带上当前内联文本,
            // 让 TSF 的 apply_reply 重写同一份预编辑而不是把它清掉。
            let mut reply = neutral(state);
            if let Some(published) = state.session(session).map(|handle| handle.published()) {
                reply.preedit = published.view.preedit.clone();
                fill_view(&mut reply, &published.view);
            }
            reply
        }
        Request::Focus { session, focused } => {
            // 失焦通知可能晚于新窗口的聚焦通知;只允许聚焦者抢占 active,
            // 失焦只清理自己,避免候选窗跟到已经切走的旧输入框。
            if focused {
                state.activate(session);
            } else if state.active.load(Ordering::Relaxed) == session {
                state.activate(0);
            }
            let mut reply = neutral(state);
            if let Some(handle) = state.session(session) {
                let mut s = handle.lock();
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
                    reply = reply_from(&s, &events);
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
                refresh_view(&mut s);
                fill_view(&mut reply, &s.view);
                // 清组合/换输入框后,待写回的内联文本必须跟上当前状态。
                sync_ui_reply(&mut s);
                if handle.publish(&s) {
                    touch_view(state);
                }
            }
            reply
        }
        Request::CompositionTerminated { session } => {
            let _ = state.with_session(session, |s| {
                let _ = s.engine.process(Key::Esc);
                // 已点选、还没写回的文本不能丢;下次按键/Sync 继续带上。
                crate::settle::cancel(state, session);
                refresh_view(s);
                sync_ui_reply(s);
            });
            neutral(state)
        }
        Request::Caret {
            session,
            left,
            top,
            right,
            bottom,
        } => {
            state.activate(session);
            let _ = state.with_session(session, |s| {
                s.view.caret = Some((left, top, right, bottom));
                // 组合已经建立、上下文也可能刚上报;在这里排停顿任务最可靠。
                crate::settle::schedule(state, session, s);
            });
            Reply::default()
        }
        Request::Pick { session, slot } => {
            state.activate(session);
            let _ = state.with_session(session, |s| {
                let before = s.engine.revision();
                let events = s.engine.pick_in_page(slot);
                for event in &events {
                    if let Event::Action(id) = event {
                        invoke_extension(state, id);
                    }
                }
                if s.engine.revision() != before {
                    crate::sync_rerank::schedule(state, session, s);
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
            });
            Reply::default()
        }
        Request::Pin { session, slot } => {
            state.activate(session);
            let _ = state.with_session(session, |s| {
                s.engine.pin_in_page(slot);
                refresh_view(s);
                crate::settle::schedule(state, session, s);
            });
            Reply::default()
        }
        Request::Demote { session, slot } => {
            state.activate(session);
            let _ = state.with_session(session, |s| {
                s.engine.demote_in_page(slot);
                refresh_view(s);
                crate::settle::schedule(state, session, s);
            });
            Reply::default()
        }
        Request::PickIndex { session, index } => {
            state.activate(session);
            let mut reply = Reply::default();
            if let Some(out) = state.with_session(session, |s| {
                let before = s.engine.revision();
                let events = s.engine.pick(index);
                for event in &events {
                    if let Event::Action(id) = event {
                        invoke_extension(state, id);
                    }
                }
                if s.engine.revision() != before {
                    crate::sync_rerank::schedule(state, session, s);
                }
                // 宿主自绘候选时由 TSF 直接收走上屏文本并写进文档,
                // 不走候选窗的 Sync 队列。
                let mut reply = reply_from(s, &events);
                refresh_view(s);
                fill_view(&mut reply, &s.view);
                crate::settle::schedule(state, session, s);
                reply
            }) {
                reply = out;
            }
            reply
        }
        Request::Finalize { session } => {
            state.activate(session);
            let Some(reply) = state.with_session(session, |s| {
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
                fill_view(&mut reply, &s.view);
                crate::settle::schedule(state, session, s);
                reply
            }) else {
                return neutral(state);
            };
            reply
        }
        Request::SelectIndex { session, index } => {
            state.activate(session);
            let mut reply = Reply::default();
            let _ = state.with_session(session, |s| {
                if s.engine.set_highlight(index) {
                    // 用户正在挑候选,停顿重排不能把高亮挪走。
                    crate::settle::cancel(state, session);
                    refresh_view(s);
                    sync_ui_reply(s);
                }
                fill_view(&mut reply, &s.view);
                // 宿主自绘候选时,高亮变化要同步到内联预编辑;否则文档里
                // 还是旧候选,点选前后看到的文本对不上。
                reply.preedit = s
                    .engine
                    .composition()
                    .map(|c| c.display.clone())
                    .unwrap_or_default();
                reply.chinese = s.engine.mode() == Mode::Chinese;
            });
            reply
        }
        Request::Page { session, delta } => {
            state.activate(session);
            let _ = state.with_session(session, |s| {
                if s.engine.page_by(delta) {
                    // 用户正在翻看候选,别让停顿重排回来把当前页换掉。
                    crate::settle::cancel(state, session);
                    refresh_view(s);
                    sync_ui_reply(s);
                }
            });
            Reply::default()
        }
        Request::UiMode {
            session,
            host_draws,
        } => {
            let _ = state.with_session(session, |s| {
                s.host_draws = host_draws;
            });
            neutral(state)
        }
        Request::ReloadConfig => {
            if let Some(path) = &state.config_path {
                let cfg = config::load(path);
                apply_config(state, cfg);
            }
            Reply::default()
        }
        Request::TogglePunct => {
            let before = state
                .config
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let cfg = AppConfig {
                chinese_punct: !before.chinese_punct,
                ..before.clone()
            };
            // Ctrl+. 是隐藏快捷键;借模式徽标短暂显示「，。」或「,.」,让用户知道切换结果。
            let label = if cfg.chinese_punct { "，。" } else { ",." };
            println!("切换中文标点 → {}", cfg.chinese_punct);
            match &state.config_path {
                Some(path) => match config::save(path, &before, &cfg) {
                    Ok(()) => println!("配置已保存 {}", path.display()),
                    Err(e) => println!("保存配置 {} 失败: {e}", path.display()),
                },
                None => println!("切换中文标点未保存: 没有配置路径"),
            }
            apply_config(state, cfg);
            show_hud(state, label, None);
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
            apply_config(state, cfg);
            group_reply(&state.user)
        }
        Request::FuzzyStats => group_reply(&state.user),
        Request::Status => {
            let sync_latency = state.rerank_sync_metrics.snapshot();
            let settle_latency = state.rerank_settle_metrics.snapshot();
            Reply {
                status: Some(EngineStatus {
                    dict_words: state.dict.len(),
                    dict_source: state.dict_source.to_string(),
                    gram: state.gram.is_some(),
                    rerank8m: state.sync_model.is_some(),
                    rerank25m: state.settled_model.is_some(),
                    rerank_sync: RerankLatency {
                        count: sync_latency.count,
                        p50_us: sync_latency.p50_us,
                        p95_us: sync_latency.p95_us,
                        p99_us: sync_latency.p99_us,
                        max_us: sync_latency.max_us,
                        mean_us: sync_latency.mean_us,
                    },
                    rerank_settle: RerankLatency {
                        count: settle_latency.count,
                        p50_us: settle_latency.p50_us,
                        p95_us: settle_latency.p95_us,
                        p99_us: settle_latency.p99_us,
                        max_us: settle_latency.max_us,
                        mean_us: settle_latency.mean_us,
                    },
                    server_version: env!("CARGO_PKG_VERSION").to_string(),
                }),
                ..Reply::default()
            }
        }
        Request::Ext { id, method, params } => {
            let result = match method.as_str() {
                "settings.get" => settings_get(state, &id),
                "settings.set" => settings_set(state, &id, params),
                "commands.list" => commands_list(state),
                "command.run" => command_run(state, &id),
                _ => Err(format!(
                    "扩展方法 {method:?} 不存在;宿主提供 settings.get / settings.set / commands.list / command.run"
                )),
            };
            let ext = match result {
                Ok(value) => lueying_protocol::ExtReply {
                    ok: true,
                    value,
                    error: String::new(),
                },
                Err(error) => lueying_protocol::ExtReply {
                    ok: false,
                    value: serde_json::Value::Null,
                    error,
                },
            };
            Reply {
                ext: Some(ext),
                ..Reply::default()
            }
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
            if state
                .sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&session)
                .is_some()
            {
                println!("会话 {session} 下线");
            }
            if state.active.load(Ordering::Relaxed) == session {
                state.activate(0);
            }
            crate::settle::cancel(state, session);
            touch_view(state);
            Reply::default()
        }
    }
}

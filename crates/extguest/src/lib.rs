//! 掠影 Wasm 扩展的 Rust SDK(WASI 0.3 组件 ABI)。
//!
//! 扩展作者实现 [`Extension`],用 [`export_component!`] 声明命令;产物是
//! 标准 WebAssembly 组件,宿主按 WIT 契约 `lueying:ext@2.0.0` 类型化调用:
//! 命令收参数 JSON 回结果 JSON,外观收场景 JSON 回补丁 JSON。命令与服务
//! 都是同步调用,宿主在扩展自己的 worker 上阻塞等待。
//!
//! 宿主服务走 WIT 导入(编译期检查);清单 `needs` 里没声明的服务在链接
//! 时就通不过。服务失败直接是 `Err`;宿主也盯着这条:扩展吞掉服务错误仍
//! 返回成功,宿主照样把这次调用判失败,失败不会被静默放过。
//!
//! 打包:先编 `wasm32-unknown-unknown`,再用 `wasm-tools component new`
//! 包成组件放进扩展目录。非 Wasm 目标(宿主侧测试)上的服务调用是返回
//! `Err` 的桩,方便在普通 `cargo test` 里检查作者逻辑。

#![deny(missing_docs)]

pub use lueying_extproto::{
    AsrText, AudioRecording, ClipboardHistory, ClipboardHistoryItem, ClipboardText, Delivered,
    FETCH_DEFAULT_LIMIT, FETCH_DEFAULT_TIMEOUT_MS, FetchArgs, FetchResult, OcrText, PanelResult,
    SCENE_API, SceneIn, ScenePatch, SelectedText, Shot, StoreKeys, StoreValue,
};

/// 扩展入口。
///
/// 宿主只调用清单里声明过的命令;`appearance` 只在清单 `appearance: true`
/// 时被派发。实现类型必须能放进静态 `Mutex`,也就是 `Send`。
pub trait Extension: Send + Default {
    /// 初始化;返回 `Err` 会让扩展加载失败。
    fn init(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// 执行一条命令;参数与返回值都是 JSON 文本。
    fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
        Err(format!("扩展没有命令 {id}"))
    }

    /// 返回一帧外观补丁;`Ok(None)` 表示这一帧不改。
    fn appearance(&mut self, _scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        Ok(None)
    }
}

/// 组件 ABI 的扩展导出(WASI 0.3 Component Model)。
///
/// 用法:`lueying_extguest::export_component!(MyExt, commands: [show])`。
/// 产物是标准 WebAssembly 组件,服务调用走 WIT 类型化接口(编译期检查);
/// 服务失败直接是 `Err`。作者 crate 需要依赖 `wit-bindgen` 与
/// `serde_json`(生成的绑定与外观引用它们)。
#[macro_export]
#[doc(hidden)]
macro_rules! __lueying_export_component {
    ($ty:ty, $wit:literal) => {
        // 非 wasm 目标:保活作者扩展类型与其 Extension 实现,宿主侧单测可
        // 直接驱动逻辑(静态 Mutex 探针保活)。
        #[cfg(not(target_arch = "wasm32"))]
        const _: () = {
            static __LUEYING_EXTENSION: ::std::sync::Mutex<::std::option::Option<$ty>> =
                ::std::sync::Mutex::new(::std::option::Option::None);
            #[allow(dead_code)]
            fn __lueying_probe() {
                let mut guard = __LUEYING_EXTENSION.lock().unwrap();
                guard.get_or_insert_with(<$ty as ::std::default::Default>::default);
            }
        };

        // 生成导入与导出;契约按相对路径取(第一方扩展与 extwasm 同仓)。
        #[cfg(target_arch = "wasm32")]
        ::wit_bindgen::generate!({
            path: $wit,
            world: "extension",
        });

        #[cfg(target_arch = "wasm32")]
        static __LUEYING_EXTENSION: ::std::sync::Mutex<::std::option::Option<$ty>> =
            ::std::sync::Mutex::new(::std::option::Option::None);

        // —— 生成 record ↔ extproto record,字段逐一对位 ——
        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_shot(
            v: lueying::ext::capture::ShotResult,
        ) -> $crate::Shot {
            $crate::Shot { file: v.file, width: v.width, height: v.height }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_ocr(v: lueying::ext::ocr::RecognizeResult) -> $crate::OcrText {
            $crate::OcrText { text: v.text, lang: v.lang }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_deliver(
            v: lueying::ext::output::DeliverResult,
        ) -> $crate::Delivered {
            $crate::Delivered { delivered: v.delivered }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_panel(
            v: lueying::ext::panel::ShowResult,
        ) -> $crate::PanelResult {
            $crate::PanelResult { selected: v.selected }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_clipboard_text(
            v: lueying::ext::clipboard::ReadResult,
        ) -> $crate::ClipboardText {
            $crate::ClipboardText { text: v.text }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_clipboard_history(
            v: lueying::ext::clipboard::HistoryResult,
        ) -> $crate::ClipboardHistory {
            $crate::ClipboardHistory {
                items: v
                    .items
                    .into_iter()
                    .map(|item| $crate::ClipboardHistoryItem {
                        text: item.text,
                        time: item.time,
                        tag: item.tag,
                    })
                    .collect(),
            }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_audio(v: lueying::ext::audio::RecordResult) -> $crate::AudioRecording {
            $crate::AudioRecording { file: v.file, seconds: v.seconds }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_asr(v: lueying::ext::asr::RecognizeResult) -> $crate::AsrText {
            $crate::AsrText { text: v.text }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_store_value(v: lueying::ext::store::GetResult) -> $crate::StoreValue {
            $crate::StoreValue { value: v.value }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_store_keys(v: lueying::ext::store::ListResult) -> $crate::StoreKeys {
            $crate::StoreKeys { keys: v.keys }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_fetch(v: lueying::ext::network::FetchResult) -> $crate::FetchResult {
            $crate::FetchResult { status: v.status, body: v.body }
        }

        #[cfg(target_arch = "wasm32")]
        fn __lueying_conv_selected(
            v: lueying::ext::context::SelectionResult,
        ) -> $crate::SelectedText {
            $crate::SelectedText { text: v.text }
        }

        // —— 导出面:init / command / appearance,委托给作者扩展 ——
        #[cfg(target_arch = "wasm32")]
        struct __LueyingEntry;

        #[cfg(target_arch = "wasm32")]
        impl exports::entry::Guest for __LueyingEntry {
            fn init() -> ::std::result::Result<(), ::std::string::String> {
                let mut slot = __LUEYING_EXTENSION.lock().unwrap();
                if slot.is_some() {
                    return Ok(());
                }
                *slot = Some(<$ty as ::std::default::Default>::default());
                match slot.as_mut().unwrap().init() {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        *slot = ::std::option::Option::None;
                        Err(error)
                    }
                }
            }

            fn command(
                id: ::std::string::String,
                args: ::std::string::String,
            ) -> ::std::result::Result<::std::string::String, ::std::string::String> {
                let mut slot = __LUEYING_EXTENSION.lock().unwrap();
                let extension = slot
                    .as_mut()
                    .ok_or_else(|| ::std::string::String::from("扩展未初始化"))?;
                extension.command(&id, &args)
            }

            fn appearance(
                scene_json: ::std::string::String,
            ) -> ::std::result::Result<
                ::std::option::Option<::std::string::String>,
                ::std::string::String,
            > {
                let scene: $crate::SceneIn = ::serde_json::from_str(&scene_json)
                    .map_err(|error| format!("场景解析失败: {error}"))?;
                let mut slot = __LUEYING_EXTENSION.lock().unwrap();
                let extension = slot
                    .as_mut()
                    .ok_or_else(|| ::std::string::String::from("扩展未初始化"))?;
                match extension.appearance(&scene)? {
                    Some(patch) => {
                        let json = ::serde_json::to_string(&patch)
                            .map_err(|error| format!("外观补丁序列化失败: {error}"))?;
                        Ok(Some(json))
                    }
                    None => Ok(None),
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        export!(__LueyingEntry);

        // —— 作者面的服务外观:签名与 v1 相同,作者代码零改动 ——
        pub fn capture_shot() -> ::std::result::Result<$crate::Shot, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_shot(lueying::ext::capture::shot(lueying::ext::capture::ShotOptions { region: None })?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn ocr_recognize(
            file: &str,
            lang: Option<&str>,
        ) -> ::std::result::Result<$crate::OcrText, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_ocr(lueying::ext::ocr::recognize(
                    &lueying::ext::ocr::RecognizeOptions {
                        file: file.to_string(),
                        lang: lang.map(str::to_string),
                        engine: None,
                    },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn output_deliver(
            text: &str,
        ) -> ::std::result::Result<$crate::Delivered, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_deliver(lueying::ext::output::deliver(
                    &lueying::ext::output::DeliverOptions { text: text.to_string() },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn panel_show(
            title: &str,
            items: &[::std::string::String],
        ) -> ::std::result::Result<$crate::PanelResult, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_panel(lueying::ext::panel::show(
                    &lueying::ext::panel::ShowOptions {
                        title: title.to_string(),
                        items: items.to_vec(),
                    },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn clipboard_read() -> ::std::result::Result<$crate::ClipboardText, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_clipboard_text(lueying::ext::clipboard::read()?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn clipboard_write(text: &str) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::clipboard::write(&lueying::ext::clipboard::WriteOptions {
                    text: text.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn clipboard_history(
            limit: usize,
        ) -> ::std::result::Result<$crate::ClipboardHistory, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_clipboard_history(lueying::ext::clipboard::history(
                    lueying::ext::clipboard::HistoryOptions { limit: Some(limit as u32) },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn clipboard_remember(
            text: &str,
            tag: &str,
        ) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::clipboard::remember(&lueying::ext::clipboard::RememberOptions {
                    text: text.to_string(),
                    tag: Some(tag.to_string()),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn clipboard_history_clear() -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::clipboard::clear()?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn audio_record(
            seconds: u32,
        ) -> ::std::result::Result<$crate::AudioRecording, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_audio(lueying::ext::audio::record(
                    lueying::ext::audio::RecordOptions { seconds: Some(seconds) },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn asr_recognize(
            file: &str,
        ) -> ::std::result::Result<$crate::AsrText, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_asr(lueying::ext::asr::recognize(
                    &lueying::ext::asr::RecognizeOptions { file: file.to_string() },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn store_get(
            key: &str,
        ) -> ::std::result::Result<$crate::StoreValue, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_store_value(lueying::ext::store::get(
                    &lueying::ext::store::GetOptions { key: key.to_string() },
                )?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn store_set(key: &str, value: &str) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::store::set(&lueying::ext::store::SetOptions {
                    key: key.to_string(),
                    value: value.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn store_remove(key: &str) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::store::remove(&lueying::ext::store::RemoveOptions {
                    key: key.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn store_list() -> ::std::result::Result<$crate::StoreKeys, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_store_keys(lueying::ext::store::list()?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn network_fetch(
            options: &$crate::FetchArgs,
        ) -> ::std::result::Result<$crate::FetchResult, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                let args = lueying::ext::network::FetchOptions {
                    url: options.url.clone(),
                    method: options.method.clone(),
                    headers: options.headers.clone(),
                    body: options.body.clone(),
                    limit: options.limit,
                    timeout_ms: options.timeout_ms,
                };
                Ok(__lueying_conv_fetch(lueying::ext::network::fetch(&args)?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn context_selection() -> ::std::result::Result<$crate::SelectedText, ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                Ok(__lueying_conv_selected(lueying::ext::context::selection()?))
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn notify_show(title: &str, body: &str) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::notify::show(&lueying::ext::notify::ShowOptions {
                    title: title.to_string(),
                    body: body.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn hotkey_register(
            id: &str,
            modifiers: &[&str],
            key: &str,
        ) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::hotkey::register(&lueying::ext::hotkey::RegisterOptions {
                    id: id.to_string(),
                    modifiers: modifiers.iter().map(|m| m.to_string()).collect(),
                    key: key.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn hotkey_unregister(id: &str) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::hotkey::unregister(&lueying::ext::hotkey::UnregisterOptions {
                    id: id.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }

        pub fn hotkey_press(id: &str) -> ::std::result::Result<(), ::std::string::String> {
            #[cfg(target_arch = "wasm32")]
            {
                lueying::ext::hotkey::press(&lueying::ext::hotkey::PressOptions {
                    id: id.to_string(),
                })?;
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                Err(::std::string::String::from("非 wasm 目标没有宿主服务"))
            }
        }
    };
}


/// 组件 ABI 的扩展导出:分发到内部实现。
///
/// 第一方扩展(在 `extensions/<id>/` 下)直接用默认契约路径;仓外或深层
/// 目录的 crate 用 `wit:` 给出相对本 crate 的 `lueying.wit` 路径。
#[macro_export]
macro_rules! export_component {
    ($ty:ty, wit: $wit:literal $(, commands: [$($command:ident),* $(,)?])? $(,)?) => {
        $crate::__lueying_export_component!($ty, $wit)
    };
    ($ty:ty $(, commands: [$($command:ident),* $(,)?])? $(,)?) => {
        $crate::__lueying_export_component!($ty, "../../crates/extwasm/lueying.wit")
    };
}

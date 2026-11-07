//! 组件缝验证桩的 guest。
//!
//! hello 调宿主 ping 再拼回名字:一条调用同时踩「guest→宿主」与
//! 「宿主→guest」两个方向,燃料与阻塞调用由宿主侧测试断言。

wit_bindgen::generate!({
    path: "../../spike.wit",
    world: "spike",
});

struct SpikeGuest;

impl exports::lueying::spike::entry::Guest for SpikeGuest {
    fn hello(name: String) -> Result<String, String> {
        let pong = lueying::spike::host::ping();
        Ok(format!("{name} ← {pong}"))
    }
}

export!(SpikeGuest);

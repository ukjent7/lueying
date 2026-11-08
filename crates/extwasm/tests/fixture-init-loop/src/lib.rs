//! 扩展宿主测试客人:init 死循环。
//!
//! 清单声明外观即可加载到 init;init 烧穿初始化燃料,宿主必须在加载期
//! 拒绝它而不是挂死(见 e2e 的 `bad_packs`)。

use lueying_extguest::Extension;

#[derive(Default)]
struct LoopInit;

impl Extension for LoopInit {
    fn init(&mut self) -> Result<(), String> {
        loop {
            std::hint::black_box(1u64.wrapping_add(1));
        }
    }
}

lueying_extguest::export_component!(LoopInit, wit: "../../../extwasm/lueying.wit");

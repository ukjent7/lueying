//! Skia 候选窗的用户效果:加载、目录指纹与热重载。
//!
//! 效果在加载期编译,坏文件只废掉自己的槽位;窗口线程每秒看一次目录
//! 指纹,用户改完 `.sksl` 一秒内生效,不用重启输入法。

use lueying_render::effect::{EffectSet, Slot};
use lueying_server::config;

use crate::imp::State;

/// 窗口线程独占的效果状态:编译好的效果集加目录指纹。
pub(super) struct Effects {
    /// 编译好的效果;空集合就是素色。
    compiled: EffectSet,
    /// 目录指纹;变了才重新加载。
    stamp: u64,
}

impl Effects {
    /// 读一遍效果目录;配置关掉、目录不存在或没有文件就是素色。
    pub fn load(state: &State) -> Self {
        Self {
            compiled: load_set(state),
            stamp: stamp(state),
        }
    }

    /// 目录或开关变了就重新加载;没变就什么都不做。
    pub fn refresh(&mut self, state: &State) {
        let stamp = stamp(state);
        if stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        self.compiled = load_set(state);
    }

    /// 当前生效的效果集。
    pub fn set(&self) -> &EffectSet {
        &self.compiled
    }
}

/// 读用户效果目录;配置关掉、目录不存在或没有文件就返回空集合。
/// 坏文件只废掉自己的槽位,错误写日志。
fn load_set(state: &State) -> EffectSet {
    if !enabled(state) {
        return EffectSet::empty();
    }
    let Some(dir) = config::effects_dir() else {
        return EffectSet::empty();
    };
    let set = EffectSet::load_dir(&dir);
    for error in set.errors() {
        println!("Skia 用户效果:{error}");
    }
    let loaded: Vec<&str> = Slot::ALL
        .iter()
        .filter(|slot| set.has(**slot))
        .map(|slot| slot.title())
        .collect();
    if !loaded.is_empty() {
        println!("Skia 用户效果已加载:{}", loaded.join("、"));
    }
    set
}

/// 效果开关;配置重载后立即生效。
fn enabled(state: &State) -> bool {
    state.config.read().unwrap_or_else(|p| p.into_inner()).effects
}

/// 效果目录的指纹:开关状态加三个文件的修改时间与大小。
fn stamp(state: &State) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    mix_hash(&mut hash, enabled(state) as u64);
    if let Some(dir) = config::effects_dir() {
        for slot in Slot::ALL {
            let path = dir.join(slot.file_name());
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|delta| delta.as_nanos() as u64)
                        .unwrap_or(0);
                    mix_hash(&mut hash, modified);
                    mix_hash(&mut hash, meta.len());
                }
                Err(_) => mix_hash(&mut hash, 0),
            }
        }
    }
    hash
}

/// FNV-1a 的一步;只用来发现文件变化,不做安全用途。
fn mix_hash(hash: &mut u64, value: u64) {
    *hash ^= value;
    *hash = hash.wrapping_mul(0x100_0000_01b3);
}

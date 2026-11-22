#![allow(unsafe_code)]

use std::sync::{OnceLock, mpsc};
use std::time::Instant;
use windows_sys::Win32::System::{
    ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
    Threading::GetCurrentProcess,
};

pub(crate) fn sample(stage: &'static str, device: Option<&wgpu::Device>) {
    static WRITER: OnceLock<Option<(Instant, mpsc::SyncSender<String>)>> = OnceLock::new();
    let Some((start, writer)) = WRITER.get_or_init(|| {
        std::env::var_os("RUNE_STAGE_TRACE")?;
        let (tx, rx) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            while let Ok(line) = rx.recv() {
                eprintln!("{line}");
            }
        });
        Some((Instant::now(), tx))
    }) else { return };
    let mut mem = PROCESS_MEMORY_COUNTERS_EX::default();
    mem.cb = std::mem::size_of_val(&mem) as u32;
    // The extended struct starts with the base counter layout, and cb supplies its size.
    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), (&mut mem as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), mem.cb) } == 0 {
        return;
    }
    let mut gpu = [0; 4];
    if let Some(device) = device {
        let counters = device.get_internal_counters().hal;
        gpu[0] = counters.texture_memory.read() as u64;
        gpu[1] = counters.buffer_memory.read() as u64;
        if let Some(report) = device.generate_allocator_report() {
            gpu[2] = report.total_allocated_bytes;
            gpu[3] = report.total_reserved_bytes;
            if matches!(stage, "renderer-ready" | "first-present" | "fourth-present" | "30th-present" | "surface-released") {
                let blocks: Vec<_> = report.blocks.iter().map(|b| b.size).collect();
                let mut names = std::collections::BTreeMap::new();
                for a in &report.allocations {
                    let (count, bytes) = names.entry(a.name.as_str()).or_insert((0, 0));
                    *count += 1;
                    *bytes += a.size;
                }
                let _ = writer.try_send(format!("rune-heaps,{stage},{blocks:?},{names:?}"));
            }
        }
    }
    let _ = writer.try_send(format!(
        "rune-stage,{},{stage},{},{},{},{},{},{},{}",
        start.elapsed().as_millis(), mem.WorkingSetSize, mem.PeakWorkingSetSize, mem.PrivateUsage,
        gpu[0], gpu[1], gpu[2], gpu[3],
    ));
}

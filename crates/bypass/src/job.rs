//! Windows Job Object 限额:旁路进程的内存与 CPU 速率硬上限。
//!
//! 进程刚启动、未通信前挂进 Job;挂不上按启动失败处理,调用方终止
//! 进程并记一次失败。Job 打开 kill-on-close 且句柄有意不关闭:进程
//! 的生命周期由 Job 兜底,宿主崩溃也会连坐收走;代价是每次调用留一
//! 个待宿主退出的 Job 句柄,按「旁路调用是用户级低频操作」的定位可
//! 以接受,常驻化时要换成子进程退出后关句柄的看护线程。

use std::os::windows::io::AsRawHandle;
use std::process::Child;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_CPU_RATE_CONTROL,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::core::PCWSTR;

use crate::ProcessLimiter;

/// Job Object 限额;克隆便宜。
#[derive(Clone, Debug, Default)]
pub struct JobLimiter {
    /// 作业内存硬上限(字节);`None` 不限。
    pub memory_bytes: Option<u64>,
    /// CPU 速率硬上限,万分比(1..=10000,10000 = 一个整核);`None` 不限。
    pub cpu_rate: Option<u32>,
}

impl JobLimiter {
    pub fn new(memory_bytes: Option<u64>, cpu_rate: Option<u32>) -> Self {
        Self {
            memory_bytes,
            cpu_rate,
        }
    }
}

impl ProcessLimiter for JobLimiter {
    fn attach(&self, process: &Child) -> Result<(), String> {
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null())
                .map_err(|error| format!("创建 Job 失败: {error}"))?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            if let Some(bytes) = self.memory_bytes {
                info.BasicLimitInformation.LimitFlags |=
                    JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                info.JobMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
                info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
            }
            if let Some(rate) = self.cpu_rate {
                info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_CPU_RATE_CONTROL;
                // CPU 速率是独立的信息类,extended limit 结构体里没有这块。
                let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
                cpu.ControlFlags =
                    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
                cpu.Anonymous.CpuRate = rate.clamp(1, 10_000);
                SetInformationJobObject(
                    job,
                    JobObjectCpuRateControlInformation,
                    &cpu as *const _ as _,
                    std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                )
                .map_err(|error| format!("设置 CPU 限额失败: {error}"))?;
            }
            // 进程随 Job 兜底:宿主崩溃或句柄泄漏,旁路进程也会被收走。
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|error| format!("设置 Job 限额失败: {error}"))?;
            AssignProcessToJobObject(job, HANDLE(process.as_raw_handle()))
                .map_err(|error| format!("进程挂入 Job 失败: {error}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod job_tests;

//! 录音与离线识别:WinMM 采 16 kHz PCM WAV,识别交给 sensevoice 扩展。

use std::path::Path;

use std::sync::Arc;

use std::time::Duration;

use lueying_extwasm::{Done, Provider};

use serde_json::json;

use super::one_method;

use super::clipboard::{now_ms, record_tagged_clipboard};

use super::ocr::NativeHosts;

/// 音频录制:16 kHz、单声道、16 位 PCM 的 WAV,给离线 ASR 用。
///
/// 直接走 WinMM `waveIn`。录音在独立线程完成,阻塞的是扩展 worker,
/// 不占服务线程;时长限制在 1~30 秒,避免一次误触长期占用麦克风。
pub struct AudioProvider;

impl Provider for AudioProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("audio", method, "record")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let seconds = parsed["seconds"].as_u64().unwrap_or(5).clamp(1, 30) as u32;
        std::thread::Builder::new()
            .name("ext-audio-record".into())
            .spawn(move || {
                let result = record_audio(seconds).and_then(|recording| {
                    serde_json::to_string(&recording)
                        .map_err(|error| format!("录音结果序列化失败: {error}"))
                });
                done.complete(result);
            })
            .map_err(|error| format!("录音线程启动失败: {error}"))?;
        Ok(())
    }
}

/// 离线语音识别:交给原生 `sensevoice` 扩展进程。
pub struct AsrProvider {
    native: Arc<NativeHosts>,
}

impl AsrProvider {
    /// 建提供者;原生扩展未安装时返回明确错误。
    pub fn new() -> Self {
        Self {
            native: Arc::new(NativeHosts::default()),
        }
    }
}

impl Default for AsrProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for AsrProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("asr", method, "recognize")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let file = parsed["file"]
            .as_str()
            .ok_or_else(|| "缺 file".to_string())?
            .to_string();
        // 录音落在临时目录,沙箱默认读不到;逐次授只读执行权限。
        lueying_extwasm::native::grant_appcontainer_read(Path::new(&file))?;
        let native = Arc::clone(&self.native);
        std::thread::Builder::new()
            .name("ext-asr".into())
            .spawn(move || {
                let result = native.call(
                    "sensevoice",
                    "recognize",
                    &json!({ "file": &file }).to_string(),
                );
                // 识别完成(成功或失败)后删掉临时录音,避免磁盘累积。
                let _ = std::fs::remove_file(&file);
                if let Ok(payload) = &result {
                    remember_asr_text(payload);
                }
                done.complete(result);
            })
            .map_err(|error| format!("语音识别线程启动失败: {error}"))?;
        Ok(())
    }
}

/// ASR 结果静默收进剪贴板历史,带 `语音` 标签;空结果不写。
fn remember_asr_text(payload: &str) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload)
        && let Some(text) = value["text"].as_str()
    {
        record_tagged_clipboard(text.trim(), "语音");
    }
}

/// WinMM 录音格式;单声道 16 kHz 16 位小端 PCM。
#[repr(C)]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    cb_size: u16,
}

/// WinMM 缓冲区头。
#[repr(C)]
struct WaveHdr {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHdr,
    reserved: usize,
}

const WAVE_MAPPER: u32 = u32::MAX;
const WHDR_DONE: u32 = 1;
const WAVE_FORMAT_PCM: u16 = 1;

#[link(name = "winmm")]
unsafe extern "system" {
    fn waveInGetNumDevs() -> u32;
    fn waveInOpen(
        device: *mut *mut core::ffi::c_void,
        device_id: u32,
        format: *const WaveFormatEx,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveInPrepareHeader(device: *mut core::ffi::c_void, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInUnprepareHeader(
        device: *mut core::ffi::c_void,
        header: *mut WaveHdr,
        size: u32,
    ) -> u32;
    fn waveInAddBuffer(device: *mut core::ffi::c_void, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInStart(device: *mut core::ffi::c_void) -> u32;
    fn waveInStop(device: *mut core::ffi::c_void) -> u32;
    fn waveInReset(device: *mut core::ffi::c_void) -> u32;
    fn waveInClose(device: *mut core::ffi::c_void) -> u32;
}

fn record_audio(seconds: u32) -> Result<lueying_extwasm::AudioRecording, String> {
    if unsafe { waveInGetNumDevs() } == 0 {
        return Err("没有可用的麦克风".to_string());
    }
    let format = WaveFormatEx {
        format_tag: WAVE_FORMAT_PCM,
        channels: 1,
        samples_per_sec: 16_000,
        avg_bytes_per_sec: 32_000,
        block_align: 2,
        bits_per_sample: 16,
        cb_size: 0,
    };
    let mut device: *mut core::ffi::c_void = std::ptr::null_mut();
    let code = unsafe { waveInOpen(&mut device, WAVE_MAPPER, &format, 0, 0, 0) };
    if code != 0 {
        return Err(format!("打开麦克风失败(WinMM {code})"));
    }
    let mut buffer = vec![0u8; seconds as usize * 32_000];
    let mut header = WaveHdr {
        data: buffer.as_mut_ptr(),
        buffer_length: buffer.len() as u32,
        bytes_recorded: 0,
        user: 0,
        flags: 0,
        loops: 0,
        next: std::ptr::null_mut(),
        reserved: 0,
    };
    let size = std::mem::size_of::<WaveHdr>() as u32;
    let result = (|| {
        let code = unsafe { waveInPrepareHeader(device, &mut header, size) };
        if code != 0 {
            return Err(format!("准备录音缓冲区失败(WinMM {code})"));
        }
        let code = unsafe { waveInAddBuffer(device, &mut header, size) };
        if code != 0 {
            unsafe {
                let _ = waveInUnprepareHeader(device, &mut header, size);
            }
            return Err(format!("挂录音缓冲区失败(WinMM {code})"));
        }
        let code = unsafe { waveInStart(device) };
        if code != 0 {
            unsafe {
                let _ = waveInUnprepareHeader(device, &mut header, size);
            }
            return Err(format!("开始录音失败(WinMM {code})"));
        }
        std::thread::sleep(Duration::from_millis(u64::from(seconds) * 1000 + 150));
        unsafe {
            let _ = waveInStop(device);
        }
        for _ in 0..100 {
            if header.flags & WHDR_DONE != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            let _ = waveInReset(device);
        }
        let recorded = header.bytes_recorded.min(buffer.len() as u32) as usize;
        let file = write_wav(&buffer[..recorded])?;
        unsafe {
            let _ = waveInUnprepareHeader(device, &mut header, size);
        }
        Ok(lueying_extwasm::AudioRecording { file, seconds })
    })();
    unsafe {
        let _ = waveInClose(device);
    }
    result
}

/// 写 16 kHz 单声道 16 位 PCM WAV 到临时目录。
fn write_wav(samples: &[u8]) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("lueying-asr-{}.wav", now_ms()));
    let data_len = samples.len() as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&WAVE_FORMAT_PCM.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(samples);
    std::fs::write(&path, bytes).map_err(|error| format!("写录音文件失败: {error}"))?;
    Ok(path.to_string_lossy().into_owned())
}

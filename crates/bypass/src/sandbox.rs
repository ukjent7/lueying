//! Windows 原生扩展沙箱:无能力的 AppContainer + Job Object。
//!
//! std 的 `Command` 传不了 AppContainer 属性,直接用 `CreateProcessW`:
//! STARTUPINFOEX 挂 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`,能力
//! 表为空(派生包 SID,不授予任何能力,无网络无设备);`CREATE_SUSPENDED`
//! 拉起,限额挂好再恢复,装不上按启动失败处理。扩展要读的文件(可执行
//! 文件本身与模型目录)显式授 `ALL APPLICATION PACKAGES` 只读执行,
//! 不赌默认 ACL。

use std::ffi::c_void;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows::Win32::Foundation::{
    CloseHandle, GENERIC_EXECUTE, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT,
    HANDLE_FLAGS, HLOCAL, LocalFree, SetHandleInformation,
};
use windows::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_FLAGS, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAceEx, AddAce, CONTAINER_INHERIT_ACE, CreateWellKnownSid,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, InitializeAcl,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    SECURITY_MAX_SID_SIZE, WinBuiltinAnyPackageSid,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};
use windows::core::{PCWSTR, PWSTR};

use super::ProcessLimiter;
use crate::Process;

/// 拉起挂起的原生扩展进程:挂限额、恢复执行,返回旁路层进程句柄。
pub(super) fn spawn_suspended(
    exe: &Path,
    args: &[String],
    readable: &[PathBuf],
    limiter: Option<&dyn ProcessLimiter>,
) -> Result<Process, String> {
    grant_read_execute(exe).map_err(|error| format!("授权可执行文件失败: {error}"))?;
    for path in readable {
        grant_read_execute(path)
            .map_err(|error| format!("授权 {} 失败: {error}", path.display()))?;
    }
    let inherit = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: true.into(),
    };
    // 标准输入输出:两端都按可继承创建,父侧两端随后清掉继承位;
    // 子进程只拿子侧端。stderr 接 NUL 设备,静默丢弃。
    let (stdin_read, stdin_write) = create_pipe(&inherit, "创建标准输入管道失败")?;
    unsafe { SetHandleInformation(stdin_write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS::default()) }
        .map_err(w32("清理标准输入继承位失败"))?;
    let (stdout_read, stdout_write) = create_pipe(&inherit, "创建标准输出管道失败")?;
    unsafe { SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS::default()) }
        .map_err(w32("清理标准输出继承位失败"))?;
    let nul = wide("NUL");
    let stderr = unsafe {
        CreateFileW(
            PCWSTR(nul.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&inherit),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(w32("打开 NUL 设备失败"))?;

    // 属性表:能力表全空 = 派生 AppContainer,无任何能力。
    let mut list_size = 0usize;
    unsafe {
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut list_size);
    }
    let mut list_buf = vec![0u8; list_size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buf.as_mut_ptr() as *mut c_void);
    unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut list_size) }
        .map_err(w32("初始化进程属性表失败"))?;
    let capabilities = SECURITY_CAPABILITIES::default();
    unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some(&capabilities as *const _ as *const c_void),
            size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )
    }
    .map_err(w32("挂安全能力属性失败"))?;

    let exe_wide = wide(&exe.as_os_str().to_string_lossy());
    let mut cmdline = Vec::new();
    quote(&exe.to_string_lossy(), &mut cmdline);
    for arg in args {
        cmdline.push(b' ' as u16);
        quote(arg, &mut cmdline);
    }
    cmdline.push(0);

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin_read;
    startup.StartupInfo.hStdOutput = stdout_write;
    startup.StartupInfo.hStdError = stderr;
    startup.lpAttributeList = list;
    let mut info = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            PCWSTR(exe_wide.as_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_SUSPENDED,
            None,
            PCWSTR::null(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    unsafe { DeleteProcThreadAttributeList(list) };
    drop(list_buf);
    if let Err(error) = created {
        unsafe {
            for handle in [stdin_read, stdin_write, stdout_read, stdout_write, stderr] {
                let _ = CloseHandle(handle);
            }
        }
        return Err(format!("拉起原生扩展失败: {error}"));
    }
    // 子侧句柄父进程留副本会顶住管道 EOF,逐个关掉。
    unsafe {
        for handle in [stdin_read, stdout_write, stderr] {
            let _ = CloseHandle(handle);
        }
    }
    let mut child = unsafe { Process::from_raw(info.hProcess, stdin_write, stdout_read) };
    if let Some(limiter) = limiter
        && let Err(error) = limiter.attach(&child)
    {
        super::kill(&mut child);
        let _ = unsafe { CloseHandle(info.hThread) };
        return Err(error);
    }
    if unsafe { ResumeThread(info.hThread) } == u32::MAX {
        super::kill(&mut child);
        let _ = unsafe { CloseHandle(info.hThread) };
        return Err("恢复原生扩展线程失败".to_string());
    }
    let _ = unsafe { CloseHandle(info.hThread) };
    Ok(child)
}

fn create_pipe(
    attributes: &SECURITY_ATTRIBUTES,
    context: &'static str,
) -> Result<(HANDLE, HANDLE), String> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    unsafe { CreatePipe(&mut read, &mut write, Some(attributes), 0) }.map_err(w32(context))?;
    Ok((read, write))
}

/// 给路径授 `ALL APPLICATION PACKAGES` 只读执行;目录连同子项继承。
/// 幂等:已有同 SID 的允许 ACE 就跳过,不重复追加。
pub(super) fn grant_read_execute(path: &Path) -> Result<(), String> {
    let text = wide(&path.as_os_str().to_string_lossy());
    unsafe {
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let status = GetNamedSecurityInfoW(
            PCWSTR(text.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        );
        if status.0 != 0 {
            return Err(format!("读安全描述符失败(Win32 {})", status.0));
        }
        // 安全描述符连着 DACL,一起等 LocalFree。
        let _descriptor = OwnedDescriptor(descriptor);
        let Some(dacl) = dacl.as_ref() else {
            return Err("对象没有 DACL".to_string());
        };
        let mut info = ACL_SIZE_INFORMATION::default();
        GetAclInformation(
            dacl,
            &mut info as *mut _ as *mut c_void,
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .map_err(w32("读 ACL 信息失败"))?;
        let mut sid_buf = [0u8; SECURITY_MAX_SID_SIZE as usize];
        let mut sid_size = SECURITY_MAX_SID_SIZE;
        let package = PSID(sid_buf.as_mut_ptr() as *mut c_void);
        CreateWellKnownSid(WinBuiltinAnyPackageSid, None, Some(package), &mut sid_size)
            .map_err(w32("取 ALL APPLICATION PACKAGES SID 失败"))?;
        for index in 0..info.AceCount {
            let mut ace: *mut c_void = null_mut();
            GetAce(dacl, index, &mut ace).map_err(w32("遍历 ACE 失败"))?;
            let allowed = ace as *mut ACCESS_ALLOWED_ACE;
            // 只认允许型 ACE(ACCESS_ALLOWED_ACE_TYPE);已有同 SID 的就跳过。
            if (*allowed).Header.AceType == 0u8
                && EqualSid(PSID((&raw mut (*allowed).SidStart).cast()), package).is_ok()
            {
                return Ok(());
            }
        }
        let new_len =
            info.AclBytesInUse as usize + size_of::<ACCESS_ALLOWED_ACE>() + sid_size as usize;
        let mut new_buf = vec![0u8; new_len];
        let new_acl = new_buf.as_mut_ptr() as *mut ACL;
        InitializeAcl(new_acl, new_len as u32, ACL_REVISION).map_err(w32("建新 ACL 失败"))?;
        for index in 0..info.AceCount {
            let mut ace: *mut c_void = null_mut();
            GetAce(dacl, index, &mut ace).map_err(w32("遍历 ACE 失败"))?;
            let size = (*(ace as *const windows::Win32::Security::ACE_HEADER)).AceSize;
            AddAce(new_acl, ACL_REVISION, u32::MAX, ace, size as u32)
                .map_err(w32("复制 ACE 失败"))?;
        }
        AddAccessAllowedAceEx(
            new_acl,
            ACL_REVISION,
            ACE_FLAGS(OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0),
            (GENERIC_READ | GENERIC_EXECUTE).0,
            package,
        )
        .map_err(w32("追加允许 ACE 失败"))?;
        let status = SetNamedSecurityInfoW(
            PCWSTR(text.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        );
        if status.0 != 0 {
            return Err(format!("写安全描述符失败(Win32 {})", status.0));
        }
    }
    Ok(())
}

/// 进程安全描述符守卫:离开作用域交还 LocalFree。
struct OwnedDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for OwnedDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

/// Win32 错误码到错误文本的通用包装。
fn w32(text: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("{text}: {error}")
}

/// UTF-8 到以 NUL 结尾的 UTF-16。
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 命令行引号包裹;内嵌引号翻倍,空格与中文路径都安全。
fn quote(value: &str, out: &mut Vec<u16>) {
    out.push(b'"' as u16);
    for ch in value.chars() {
        if ch == '"' {
            out.push(b'"' as u16);
        }
        out.push(ch as u16);
    }
    out.push(b'"' as u16);
}

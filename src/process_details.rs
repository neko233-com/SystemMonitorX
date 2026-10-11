//! On-demand read-only details for one selected, exact process identity.
//!
//! Call on an asynchronous/background task when selection changes or the user
//! refreshes details. Do not run this for all processes on every monitor sample.
//! CompanyName and other strings are executable version resources, not a
//! verified Authenticode publisher. No PEB memory/layout is read.

use std::ffi::c_void;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use windows::Win32::Foundation::{
    CloseHandle, FILETIME, GetLastError, HANDLE, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL,
    STATUS_INFO_LENGTH_MISMATCH,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::{IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_UNKNOWN};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, GetPriorityClass,
    GetProcessInformation, GetProcessTimes, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS,
    IsWow64Process2, NORMAL_PRIORITY_CLASS, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, ProcessPowerThrottling,
    QueryFullProcessImageNameW, REALTIME_PRIORITY_CLASS,
};
use windows::core::{PCSTR, PCWSTR, PWSTR, w};

const MAX_COMMAND_LINE_BYTES: usize = 1024 * 1024;
const MAX_VERSION_RESOURCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct ProcessDetails {
    pub pid: u32,
    /// Exact Windows creation FILETIME in 100ns ticks, not Unix seconds.
    pub creation_ticks: u64,
    pub executable_path: Option<String>,
    /// Original Windows command-line string; it is not split/reconstructed.
    pub command_line: Option<String>,
    pub architecture: Option<String>,
    pub native_architecture: Option<String>,
    pub is_wow64: Option<bool>,
    /// No stable remote-process Win32 API exposes this; currently unavailable.
    pub current_directory: Option<String>,
    pub priority_class: Option<u32>,
    pub priority: Option<String>,
    /// Execution-speed power throttling (EcoQoS), independently read.
    pub efficiency_mode: Option<bool>,
    pub file_description: Option<String>,
    /// Unverified CompanyName text from the file's version resource.
    pub company_name: Option<String>,
    pub product_name: Option<String>,
    pub file_version: Option<String>,
    pub product_version: Option<String>,
    pub original_filename: Option<String>,
    pub file_metadata_source: Option<String>,
    pub issues: Vec<String>,
    pub read_duration_ms: u64,
}

impl ProcessDetails {
    pub fn read(pid: u32, exact_creation_ticks: u64) -> Result<Self> {
        let started = Instant::now();
        ensure!(
            pid > 0 && exact_creation_ticks > 0,
            "缺少可验证的进程 PID 或精确创建时间"
        );
        // SAFETY: a read-only, minimally privileged process handle. Ownership
        // remains live from identity verification through the final check.
        let process = OwnedProcess(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .context("无法读取此进程详情：进程已退出、访问被拒绝或受到系统保护")?,
        );
        verify_identity(process.0, exact_creation_ticks)?;
        let mut details = Self {
            pid,
            creation_ticks: exact_creation_ticks,
            ..Default::default()
        };
        match command_line(process.0) {
            Ok(command) => details.command_line = Some(command),
            Err(error) => details.issues.push(format!("命令行不可用：{error:#}")),
        }
        match process_architecture(process.0) {
            Ok((process_machine, native_machine, wow64)) => {
                details.architecture = Some(machine_name(process_machine));
                details.native_architecture = Some(machine_name(native_machine));
                details.is_wow64 = Some(wow64);
            }
            Err(error) => details.issues.push(format!("进程架构不可用：{error:#}")),
        }
        // GetCurrentDirectory exposes only the calling process, so using it for
        // this PID would silently report the monitor's own directory instead.
        details
            .issues
            .push("当前目录不可用：Windows 没有稳定公开的远程进程当前目录 API".into());
        // SAFETY: owned read-only process handle; zero means failure.
        let priority = unsafe { GetPriorityClass(process.0) };
        if priority != 0 {
            details.priority_class = Some(priority);
            details.priority = Some(priority_name(priority));
        } else {
            // SAFETY: read immediately after GetPriorityClass returned zero.
            let error = unsafe { GetLastError() };
            details
                .issues
                .push(format!("优先级不可用：Windows 错误 {}", error.0));
        }
        let mut power = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ..Default::default()
        };
        // SAFETY: correct version/size of the documented power state structure.
        let power_result = unsafe {
            GetProcessInformation(
                process.0,
                ProcessPowerThrottling,
                (&mut power as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        match power_result {
            Ok(()) => {
                details.efficiency_mode = Some(
                    power.ControlMask & power.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                        != 0,
                )
            }
            Err(error) => details
                .issues
                .push(format!("效率模式 / EcoQoS 不可用：{error}")),
        }
        match image_path(process.0) {
            Ok(path) => {
                match executable_version(&path) {
                    Ok(version) => {
                        details.file_description = version.description;
                        details.company_name = version.company;
                        details.product_name = version.product;
                        details.file_version = version.file_version;
                        details.product_version = version.product_version;
                        details.original_filename = version.original_filename;
                        details.file_metadata_source =
                            Some("可执行文件 / MUI 版本资源；公司文字未验证数字签名".into());
                    }
                    Err(error) => details
                        .issues
                        .push(format!("文件版本资源不可用：{error:#}")),
                }
                details.executable_path = Some(path);
            }
            Err(error) => details
                .issues
                .push(format!("可执行文件路径不可用：{error:#}")),
        }
        // Reject results if the selected process exited during file metadata
        // work. Holding the handle prevents any query from switching to a new
        // process that happens to reuse its numeric PID.
        verify_identity(process.0, exact_creation_ticks)?;
        details.read_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        Ok(details)
    }
}

struct OwnedProcess(HANDLE);

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: one successful OpenProcess reference is owned here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn verify_identity(handle: HANDLE, expected: u64) -> Result<()> {
    let (mut creation, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: live owned handle and valid writable FILETIME storage.
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
        .context("无法核验进程精确创建时间")?;
    let actual = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    ensure!(
        actual == expected,
        "进程身份已改变（PID 被重用或选择已过期），未读取详情"
    );
    ensure!(
        exit.dwHighDateTime == 0 && exit.dwLowDateTime == 0,
        "进程已退出，详情结果已过期"
    );
    Ok(())
}

fn image_path(handle: HANDLE) -> Result<String> {
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    // SAFETY: correct output capacity; Win32 returns actual UTF-16 length.
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .context("QueryFullProcessImageNameW 失败")?;
    ensure!(
        length > 0 && length as usize <= buffer.len(),
        "系统返回的进程路径长度无效"
    );
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    ensure!(
        std::path::Path::new(&path).is_absolute(),
        "系统未返回完整可执行文件路径"
    );
    Ok(path)
}

fn process_architecture(handle: HANDLE) -> Result<(u16, u16, bool)> {
    let mut process_machine = IMAGE_FILE_MACHINE::default();
    let mut native_machine = IMAGE_FILE_MACHINE::default();
    // SAFETY: owned handle and valid machine-code outputs. Supported since
    // Windows 10 1709, within this application's supported Windows releases.
    unsafe { IsWow64Process2(handle, &mut process_machine, Some(&mut native_machine)) }
        .context("IsWow64Process2 失败")?;
    let wow64 = process_machine != IMAGE_FILE_MACHINE_UNKNOWN;
    Ok((
        if wow64 {
            process_machine.0
        } else {
            native_machine.0
        },
        native_machine.0,
        wow64,
    ))
}

fn machine_name(machine: u16) -> String {
    match machine {
        0x014c => "x86（32 位）".into(),
        0x8664 => "x64（64 位）".into(),
        0xaa64 => "ARM64".into(),
        0xa641 => "ARM64EC".into(),
        0xa64e => "ARM64X".into(),
        0x01c4 => "ARM Thumb-2（32 位）".into(),
        value => format!("机器架构 0x{value:04X}"),
    }
}

fn priority_name(priority: u32) -> String {
    match priority {
        value if value == IDLE_PRIORITY_CLASS.0 => "低".into(),
        value if value == BELOW_NORMAL_PRIORITY_CLASS.0 => "低于正常".into(),
        value if value == NORMAL_PRIORITY_CLASS.0 => "正常".into(),
        value if value == ABOVE_NORMAL_PRIORITY_CLASS.0 => "高于正常".into(),
        value if value == HIGH_PRIORITY_CLASS.0 => "高".into(),
        value if value == REALTIME_PRIORITY_CLASS.0 => "实时".into(),
        value => format!("优先级 0x{value:08X}"),
    }
}

type NtQueryProcess = unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CommandLineUnicode {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

fn command_line(handle: HANDLE) -> Result<String> {
    // The safe variable-buffer query is also used by sysinfo's Windows
    // get_cmd_line_new/process variable-size helper (sysinfo 0.39.6 source).
    // Microsoft's NtQueryInformationProcess documentation recommends dynamic
    // linking so unavailable native APIs can be handled without a load failure.
    // This copies the command-line data via class 60, never reads a remote PEB.
    // SAFETY: ntdll is already loaded in every Win32 process; borrowed module.
    let module = unsafe { GetModuleHandleW(w!("ntdll.dll")) }.context("系统未提供 ntdll.dll")?;
    // SAFETY: fixed NUL-terminated export name and exact NT native function ABI.
    let query =
        unsafe { GetProcAddress(module, PCSTR(c"NtQueryInformationProcess".as_ptr().cast())) }
            .context("系统未提供 NtQueryInformationProcess")?;
    // SAFETY: public native function has precisely this HANDLE/class/buffer ABI.
    let query = unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, NtQueryProcess>(query)
    };
    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
    let mut needed = 0_u32;
    // SAFETY: a zero-sized discovery query only writes the required size.
    let status = unsafe {
        query(
            handle,
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    ensure!(
        [
            STATUS_INFO_LENGTH_MISMATCH.0,
            STATUS_BUFFER_TOO_SMALL.0,
            STATUS_BUFFER_OVERFLOW.0
        ]
        .contains(&status),
        "NtQueryInformationProcess 不支持或拒绝此命令行查询（NTSTATUS 0x{:08X}）",
        status as u32
    );
    for _ in 0..3 {
        ensure!(
            needed as usize >= size_of::<CommandLineUnicode>()
                && needed as usize <= MAX_COMMAND_LINE_BYTES,
            "系统返回的命令行缓冲区长度无效"
        );
        let capacity = needed as usize;
        let mut storage = vec![0_usize; capacity.div_ceil(size_of::<usize>())];
        let mut returned = 0_u32;
        // SAFETY: aligned and correctly sized output storage remains live while
        // the returned UNICODE_STRING header and inline UTF-16 are decoded.
        let status = unsafe {
            query(
                handle,
                PROCESS_COMMAND_LINE_INFORMATION,
                storage.as_mut_ptr().cast(),
                capacity as u32,
                &mut returned,
            )
        };
        if [
            STATUS_INFO_LENGTH_MISMATCH.0,
            STATUS_BUFFER_TOO_SMALL.0,
            STATUS_BUFFER_OVERFLOW.0,
        ]
        .contains(&status)
        {
            needed = returned;
            continue;
        }
        ensure!(
            status >= 0,
            "命令行查询被拒绝（NTSTATUS 0x{:08X}）",
            status as u32
        );
        ensure!(
            returned as usize <= capacity,
            "命令行结果长度超出系统缓冲区"
        );
        return decode_command_line(&storage, capacity);
    }
    bail!("命令行在查询期间持续变化，无法取得完整结果")
}

fn decode_command_line(storage: &[usize], capacity: usize) -> Result<String> {
    ensure!(
        capacity >= size_of::<CommandLineUnicode>() && capacity <= size_of_val(storage),
        "命令行缓冲区无效"
    );
    // SAFETY: aligned header-sized local output buffer, not remote memory.
    let value = unsafe { storage.as_ptr().cast::<CommandLineUnicode>().read() };
    let bytes = usize::from(value.length);
    ensure!(
        bytes.is_multiple_of(2) && value.length <= value.maximum_length,
        "命令行 UTF-16 长度无效"
    );
    if bytes == 0 {
        return Ok(String::new());
    }
    let base = storage.as_ptr() as usize;
    let pointer = value.buffer as usize;
    ensure!(
        pointer.is_multiple_of(2) && pointer >= base + size_of::<CommandLineUnicode>(),
        "命令行指针未引用本地结果"
    );
    ensure!(
        pointer
            .checked_sub(base)
            .and_then(|offset| offset.checked_add(bytes))
            .is_some_and(|end| end <= capacity),
        "命令行字符串超出本地结果范围"
    );
    // SAFETY: pointer/alignment/byte count are fully validated inside storage.
    let units = unsafe { std::slice::from_raw_parts(value.buffer, bytes / 2) };
    Ok(String::from_utf16_lossy(units))
}

#[derive(Default)]
struct ExecutableVersion {
    description: Option<String>,
    company: Option<String>,
    product: Option<String>,
    file_version: Option<String>,
    product_version: Option<String>,
    original_filename: Option<String>,
}

fn executable_version(path: &str) -> Result<ExecutableVersion> {
    ensure!(
        !path.contains('\0') && std::path::Path::new(path).is_absolute(),
        "版本资源路径无效"
    );
    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: trusted, complete, NUL-terminated image path from process handle.
    let bytes = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
    if bytes == 0 {
        // SAFETY: read immediately after the failed version-size call.
        let error = unsafe { GetLastError() };
        bail!(
            "文件没有可读取的版本资源或访问被拒绝（Windows 错误 {}）",
            error.0
        );
    }
    ensure!(
        bytes as usize <= MAX_VERSION_RESOURCE_BYTES,
        "文件版本资源过大，未读取"
    );
    let mut storage = vec![0_u64; (bytes as usize).div_ceil(size_of::<u64>())];
    // SAFETY: aligned writable buffer has at least the requested resource size.
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(path.as_ptr()),
            None,
            bytes,
            storage.as_mut_ptr().cast(),
        )
    }
    .context("GetFileVersionInfoW 读取失败")?;
    let mut result = ExecutableVersion::default();
    if let Some((pointer, length)) = version_value(&storage, "\\")
        && length as usize >= size_of::<VS_FIXEDFILEINFO>()
        && inside_version_buffer(
            &storage,
            bytes as usize,
            pointer,
            size_of::<VS_FIXEDFILEINFO>(),
        )
    {
        // SAFETY: the full fixed version struct is inside the local resource.
        let fixed = unsafe { pointer.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
        if fixed.dwSignature == 0xFEEF_04BD {
            result.file_version =
                Some(version_number(fixed.dwFileVersionMS, fixed.dwFileVersionLS));
            result.product_version = Some(version_number(
                fixed.dwProductVersionMS,
                fixed.dwProductVersionLS,
            ));
        }
    }
    if let Some((pointer, length)) = version_value(&storage, "\\VarFileInfo\\Translation")
        && length > 0
        && length.is_multiple_of(4)
        && inside_version_buffer(&storage, bytes as usize, pointer, length as usize)
    {
        // SAFETY: translation array byte range has been validated inside buffer.
        let translations =
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) };
        for translation in translations.chunks_exact(4) {
            let language = u16::from_le_bytes([translation[0], translation[1]]);
            let codepage = u16::from_le_bytes([translation[2], translation[3]]);
            let field = |key: &str| {
                version_string(
                    &storage,
                    bytes as usize,
                    &format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\{key}"),
                )
            };
            result.description = result.description.or_else(|| field("FileDescription"));
            result.company = result.company.or_else(|| field("CompanyName"));
            result.product = result.product.or_else(|| field("ProductName"));
            result.original_filename = result
                .original_filename
                .or_else(|| field("OriginalFilename"));
            // Prefer textual version when provided, preserving vendor formatting.
            if let Some(version) = field("FileVersion") {
                result.file_version = Some(version)
            }
            if let Some(version) = field("ProductVersion") {
                result.product_version = Some(version)
            }
        }
    }
    Ok(result)
}

fn version_value(storage: &[u64], key: &str) -> Option<(*const c_void, u32)> {
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let mut pointer = std::ptr::null_mut();
    let mut length = 0_u32;
    // SAFETY: resource was obtained by GetFileVersionInfoW; key is valid and
    // outputs are writable. Any returned pointer is range checked before read.
    if !unsafe {
        VerQueryValueW(
            storage.as_ptr().cast(),
            PCWSTR(key.as_ptr()),
            &mut pointer,
            &mut length,
        )
    }
    .as_bool()
        || pointer.is_null()
    {
        return None;
    }
    Some((pointer.cast_const(), length))
}

fn inside_version_buffer(
    storage: &[u64],
    capacity: usize,
    pointer: *const c_void,
    bytes: usize,
) -> bool {
    capacity <= size_of_val(storage)
        && (pointer as usize)
            .checked_sub(storage.as_ptr() as usize)
            .and_then(|offset| offset.checked_add(bytes))
            .is_some_and(|end| end <= capacity)
}

fn version_string(storage: &[u64], capacity: usize, key: &str) -> Option<String> {
    let (pointer, characters) = version_value(storage, key)?;
    let bytes = (characters as usize).checked_mul(size_of::<u16>())?;
    if characters == 0
        || !(pointer as usize).is_multiple_of(2)
        || !inside_version_buffer(storage, capacity, pointer, bytes)
    {
        return None;
    }
    // SAFETY: VerQueryValueW string lengths are UTF-16 characters (not bytes),
    // and the complete aligned range has been validated within the resource.
    let units = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), characters as usize) };
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let string = String::from_utf16_lossy(&units[..end]).trim().to_owned();
    (!string.is_empty()).then_some(string)
}

fn version_number(ms: u32, ls: u32) -> String {
    format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // Only this test's child is terminated; no user process is touched.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn owned_child_command_line_architecture_resources_and_exact_identity() {
        let mut directory = [0_u16; 32_768];
        // SAFETY: valid bounded Windows system-directory output storage.
        let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
        assert!(length > 0 && length < directory.len());
        let program = std::path::PathBuf::from(String::from_utf16_lossy(&directory[..length]))
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let child = OwnedChild(
            Command::new(program)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 20",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; no interactive UI.
                .spawn()
                .expect("owned hidden child"),
        );
        // SAFETY: only the owned test child, minimal query rights.
        let handle = OwnedProcess(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, child.0.id()) }.unwrap(),
        );
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            .unwrap();
        let birth = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let details = ProcessDetails::read(child.0.id(), birth).expect("real owned child details");
        assert!(
            details
                .command_line
                .as_ref()
                .is_some_and(|cmd| cmd.contains("Start-Sleep -Seconds 20")),
            "{:?}",
            details.issues
        );
        assert!(details.architecture.is_some());
        assert!(details.file_description.is_some());
        assert!(details.company_name.is_some());
        assert!(details.file_version.is_some());
        assert!(details.current_directory.is_none());
        assert!(
            ProcessDetails::read(child.0.id(), birth + 1).is_err(),
            "one FILETIME tick must reject identity"
        );
        eprintln!("Owned child details: {details:?}");
    }

    #[test]
    fn owned_wow64_child_uses_native_query_without_remote_peb_layout() {
        let mut directory = [0_u16; 32_768];
        // SAFETY: valid bounded output storage for the installed system path.
        let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
        assert!(length > 0 && length < directory.len());
        let system = std::path::PathBuf::from(String::from_utf16_lossy(&directory[..length]));
        let Some(windows_directory) = system.parent() else {
            return;
        };
        let program = windows_directory
            .join("SysWOW64")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if !program.is_file() {
            return;
        } // No WOW64 installation on this OS.
        let child = OwnedChild(
            Command::new(program)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 20",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000)
                .spawn()
                .expect("owned hidden WOW64 child"),
        );
        // SAFETY: only the newly owned child is opened/read, no user PID.
        let handle = OwnedProcess(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, child.0.id()) }.unwrap(),
        );
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            .unwrap();
        let birth = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let details = ProcessDetails::read(child.0.id(), birth).unwrap();
        assert!(
            details
                .command_line
                .as_ref()
                .is_some_and(|cmd| cmd.contains("Start-Sleep -Seconds 20")),
            "{:?}",
            details.issues
        );
        assert_eq!(details.is_wow64, Some(true));
        assert!(
            details
                .architecture
                .as_ref()
                .is_some_and(|architecture| architecture.contains("x86"))
        );
        assert!(ProcessDetails::read(child.0.id(), birth + 1).is_err());
        eprintln!(
            "Owned WOW64 child command line {:?}, architecture {:?}, duration {} ms",
            details.command_line, details.architecture, details.read_duration_ms
        );
    }

    #[test]
    fn command_line_local_pointer_bounds_and_utf16_lengths_are_validated() {
        let mut storage = vec![0_usize; 8];
        let header = CommandLineUnicode {
            length: 2,
            maximum_length: 2,
            buffer: std::ptr::null(),
        };
        // SAFETY: test-owned aligned storage has room for the header.
        unsafe {
            storage
                .as_mut_ptr()
                .cast::<CommandLineUnicode>()
                .write(header)
        };
        assert!(decode_command_line(&storage, size_of_val(storage.as_slice())).is_err());
        let pointer = unsafe {
            storage
                .as_mut_ptr()
                .cast::<u8>()
                .add(size_of::<CommandLineUnicode>())
                .cast::<u16>()
        };
        unsafe { pointer.write(65) };
        let header = CommandLineUnicode {
            buffer: pointer,
            ..header
        };
        unsafe {
            storage
                .as_mut_ptr()
                .cast::<CommandLineUnicode>()
                .write(header)
        };
        assert_eq!(
            decode_command_line(&storage, size_of_val(storage.as_slice())).unwrap(),
            "A"
        );
        let header = CommandLineUnicode {
            length: 3,
            maximum_length: 4,
            ..header
        };
        unsafe {
            storage
                .as_mut_ptr()
                .cast::<CommandLineUnicode>()
                .write(header)
        };
        assert!(decode_command_line(&storage, size_of_val(storage.as_slice())).is_err());
    }

    #[test]
    fn version_buffer_bounds_machine_codes_and_numeric_versions_are_exact() {
        let storage = [0_u64; 4];
        assert!(inside_version_buffer(
            &storage,
            32,
            storage.as_ptr().cast(),
            32
        ));
        assert!(!inside_version_buffer(
            &storage,
            32,
            storage.as_ptr().cast(),
            33
        ));
        assert!(!inside_version_buffer(
            &storage,
            40,
            storage.as_ptr().cast(),
            8
        ));
        assert_eq!(version_number(0x000a_0000, 0x65f4_0001), "10.0.26100.1");
        assert!(machine_name(0x8664).contains("x64"));
        assert!(machine_name(0x014c).contains("x86"));
        assert_eq!(machine_name(0xa641), "ARM64EC");
    }
}

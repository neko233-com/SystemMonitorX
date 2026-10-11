//! Windows management operations. Resource handles are kept alive between identity
//! verification and mutation so that a recycled PID cannot target another process.

use std::{ffi::c_void, mem::size_of, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail, ensure};
use windows::Win32::System::{
    Diagnostics::ToolHelp as toolhelp, Registry as registry, RemoteDesktop as wts, Services as scm,
    SystemInformation::GetSystemTimeAsFileTime, Threading as threading,
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
            ERROR_NO_MORE_FILES, ERROR_NO_MORE_ITEMS, FILETIME, HANDLE,
        },
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::{
            Com::CoTaskMemFree,
            Environment::ExpandEnvironmentStringsW,
            Registry::{
                HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY,
                KEY_WOW64_64KEY, REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ, RegCloseKey, RegEnumValueW,
                RegOpenKeyExW,
            },
            Services::{
                CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW,
                OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW,
                SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_AUTO_START,
                SERVICE_BOOT_START, SERVICE_CONTINUE_PENDING, SERVICE_DEMAND_START,
                SERVICE_DISABLED, SERVICE_DRIVER, SERVICE_PAUSE_PENDING, SERVICE_PAUSED,
                SERVICE_QUERY_CONFIG, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATE_ALL,
                SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_SYSTEM_START, SERVICE_WIN32,
            },
            Threading::{
                ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, CreateProcessW,
                GetCurrentProcess, GetProcessTimes, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS,
                IsProcessCritical, NORMAL_PRIORITY_CLASS, OpenProcess, OpenProcessToken,
                PROCESS_ACCESS_RIGHTS, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_TERMINATE,
                STARTUPINFOW, SetPriorityClass, TerminateProcess,
            },
        },
        UI::Shell::{
            FOLDERID_CommonStartup, FOLDERID_Startup, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
        },
    },
    core::{BOOL, GUID, PCWSTR, PWSTR},
};

#[derive(Clone, Debug)]
pub struct ServiceInfo {
    pub name: String,
    pub display_name: String,
    pub status: String,
    pub pid: u32,
    pub start_type: String,
    pub configuration: Option<ServiceConfiguration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceConfiguration {
    pub binary_path: String,
    pub account: String,
    pub start_type_code: u32,
    pub service_type: u32,
}

#[derive(Clone, Debug)]
pub struct StartupEntry {
    pub name: String,
    pub command: String,
    pub source: String,
    pub enabled: Option<bool>,
    pub identity: StartupIdentity,
}

/// Opaque identity captured while enumerating an entry. It cannot be forged by
/// the UI, and is checked again immediately before an approval value is changed.
#[derive(Clone, Debug)]
pub struct StartupIdentity {
    name: String,
    origin: StartupOrigin,
    approval: ApprovalSnapshot,
}

#[derive(Clone, Debug)]
enum StartupOrigin {
    Registry {
        machine: bool,
        view32: bool,
        key: String,
        approval_key: String,
        kind: u32,
        bytes: Vec<u8>,
    },
    Folder {
        path: PathBuf,
        machine: bool,
        size: u64,
        modified: u64,
        created: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApprovalSnapshot {
    Absent,
    Binary(Vec<u8>),
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct ServiceHandle(SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

struct RegistryHandle(HKEY);

impl Drop for RegistryHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn wide(value: &str) -> Result<Vec<u16>> {
    ensure!(!value.contains('\0'), "输入包含无效的空字符");
    Ok(value.encode_utf16().chain(Some(0)).collect())
}

/// Lists live SCM entries, including driver services. Inaccessible configuration
/// is reported as such without dropping the service's available status data.
pub fn list_services() -> Result<Vec<ServiceInfo>> {
    let manager = ServiceHandle(
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE) }
            .context("无法读取 Windows 服务管理器")?,
    );
    let mut services = Vec::new();
    let mut resume = 0u32;

    // Vec<usize> supplies the alignment needed by ENUM_SERVICE_STATUS_PROCESSW.
    let mut storage = vec![0usize; (256 * 1024) / size_of::<usize>()];
    loop {
        let previous_resume = resume;
        let mut needed = 0u32;
        let mut returned = 0u32;
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                storage.as_mut_ptr().cast::<u8>(),
                storage.len() * size_of::<usize>(),
            )
        };
        let result = unsafe {
            EnumServicesStatusExW(
                manager.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32 | SERVICE_DRIVER,
                SERVICE_STATE_ALL,
                Some(buffer),
                &mut needed,
                &mut returned,
                Some(&mut resume),
                PCWSTR::null(),
            )
        };
        if let Err(error) = &result
            && error.code() != ERROR_MORE_DATA.to_hresult()
        {
            return Err(error.clone()).context("枚举 Windows 服务失败");
        }
        ensure!(
            returned as usize
                <= storage.len() * size_of::<usize>() / size_of::<ENUM_SERVICE_STATUS_PROCESSW>(),
            "Windows 返回的服务列表长度无效"
        );
        let entries = unsafe {
            std::slice::from_raw_parts(
                storage.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                returned as usize,
            )
        };
        for entry in entries {
            let name = unsafe { entry.lpServiceName.to_string() }.context("无法读取服务名称")?;
            let display_name =
                unsafe { entry.lpDisplayName.to_string() }.context("无法读取服务显示名称")?;
            let status = match entry.ServiceStatusProcess.dwCurrentState {
                SERVICE_RUNNING => "运行中",
                SERVICE_STOPPED => "已停止",
                SERVICE_START_PENDING => "正在启动",
                SERVICE_STOP_PENDING => "正在停止",
                SERVICE_CONTINUE_PENDING => "正在继续",
                SERVICE_PAUSE_PENDING => "正在暂停",
                SERVICE_PAUSED => "已暂停",
                _ => "未知状态",
            }
            .to_owned();
            let configuration = service_configuration(manager.0, &name).ok();
            let start_type = configuration
                .as_ref()
                .map(|c| service_start_label(c.start_type_code))
                .unwrap_or("无法读取配置")
                .to_owned();
            services.push(ServiceInfo {
                name,
                display_name,
                status,
                pid: entry.ServiceStatusProcess.dwProcessId,
                start_type,
                configuration,
            });
        }
        if result.is_ok() {
            break;
        }
        ensure!(
            returned > 0 || resume != previous_resume,
            "服务枚举没有返回数据，请稍后重试"
        );
    }
    services.sort_by_key(|service| service.display_name.to_lowercase());
    Ok(services)
}

fn service_configuration(manager: SC_HANDLE, name: &str) -> Result<ServiceConfiguration> {
    let name = wide(name)?;
    let service = ServiceHandle(unsafe {
        OpenServiceW(manager, PCWSTR(name.as_ptr()), SERVICE_QUERY_CONFIG)
    }?);
    let mut needed = 0u32;
    match unsafe { QueryServiceConfigW(service.0, None, 0, &mut needed) } {
        Err(error) if error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {}
        Err(error) => return Err(error.into()),
        Ok(()) => bail!("服务配置未提供缓冲区大小"),
    }
    ensure!(
        needed as usize >= size_of::<QUERY_SERVICE_CONFIGW>() && needed <= 8192,
        "服务配置大小无效"
    );
    let mut storage = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let config = storage.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    unsafe { QueryServiceConfigW(service.0, Some(config), needed, &mut needed) }?;
    let config = unsafe { &*config };
    Ok(ServiceConfiguration {
        binary_path: unsafe { config.lpBinaryPathName.to_string() }?,
        account: unsafe { config.lpServiceStartName.to_string() }?,
        start_type_code: config.dwStartType.0,
        service_type: config.dwServiceType.0,
    })
}

fn service_start_label(kind: u32) -> &'static str {
    match scm::SERVICE_START_TYPE(kind) {
        SERVICE_BOOT_START => "引导启动",
        SERVICE_SYSTEM_START => "系统启动",
        SERVICE_AUTO_START => "自动",
        SERVICE_DEMAND_START => "手动",
        SERVICE_DISABLED => "已禁用",
        _ => "未知启动类型",
    }
}

/// Reads persistent Run entries and the actual Windows known Startup folders.
/// Shortcut entries retain their .lnk path; listing never resolves or executes them.
pub fn list_startup() -> Result<Vec<StartupEntry>> {
    let mut entries = Vec::new();
    read_run_key(
        HKEY_CURRENT_USER,
        KEY_WOW64_64KEY,
        "HKCU · Run",
        &mut entries,
    )?;
    read_run_key(
        HKEY_LOCAL_MACHINE,
        KEY_WOW64_64KEY,
        "HKLM · Run (64 位)",
        &mut entries,
    )?;
    read_run_key(
        HKEY_LOCAL_MACHINE,
        KEY_WOW64_32KEY,
        "HKLM · Run (32 位)",
        &mut entries,
    )?;
    read_startup_folder(&FOLDERID_Startup, "当前用户启动文件夹", &mut entries)?;
    read_startup_folder(&FOLDERID_CommonStartup, "所有用户启动文件夹", &mut entries)?;
    entries.sort_by_key(|entry| (entry.name.to_lowercase(), entry.source.clone()));
    Ok(entries)
}

fn read_run_key(
    root: HKEY,
    view: REG_SAM_FLAGS,
    source: &str,
    entries: &mut Vec<StartupEntry>,
) -> Result<()> {
    let subkey = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run")?;
    let mut raw_key = HKEY::default();
    let result = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_READ | view,
            &mut raw_key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    result
        .ok()
        .with_context(|| format!("无法读取 {source} 启动项"))?;
    let key = RegistryHandle(raw_key);
    let mut index = 0;
    loop {
        // Registry value names are limited to 16,383 UTF-16 characters.
        let mut name = vec![0u16; 16_384];
        let mut data = vec![0u8; 4096];
        let mut kind = 0u32;
        let (name_len, data_len) = loop {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let result = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if result == ERROR_NO_MORE_ITEMS {
                return Ok(());
            }
            if result == ERROR_MORE_DATA {
                ensure!(data_len <= 1024 * 1024, "{source} 的启动项过大");
                data.resize((data_len as usize).max(data.len() * 2), 0);
                continue;
            }
            result
                .ok()
                .with_context(|| format!("枚举 {source} 启动项失败"))?;
            break (name_len as usize, data_len as usize);
        };
        index += 1;
        if kind != REG_SZ.0 && kind != REG_EXPAND_SZ.0 {
            continue;
        }
        ensure!(
            data_len % 2 == 0 && data_len <= data.len() && name_len <= name.len(),
            "{source} 的启动项格式无效"
        );
        let units: Vec<u16> = data[..data_len]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        let command = String::from_utf16_lossy(&units[..end]);
        let command = if kind == REG_EXPAND_SZ.0 {
            expand_environment(&command)?
        } else {
            command
        };
        let name = String::from_utf16_lossy(&name[..name_len]);
        let origin = StartupOrigin::Registry {
            machine: root == HKEY_LOCAL_MACHINE,
            view32: view == KEY_WOW64_32KEY,
            key: "Software\\Microsoft\\Windows\\CurrentVersion\\Run".to_owned(),
            approval_key: format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\{}",
                if view == KEY_WOW64_32KEY {
                    "Run32"
                } else {
                    "Run"
                }
            ),
            kind,
            bytes: data[..data_len].to_vec(),
        };
        let approval = read_approval(&origin, &name).unwrap_or(ApprovalSnapshot::Unreadable);
        entries.push(StartupEntry {
            name: name.clone(),
            command,
            source: source.to_owned(),
            enabled: approval_enabled(&approval),
            identity: StartupIdentity {
                name: name.clone(),
                origin,
                approval,
            },
        });
    }
}

fn expand_environment(value: &str) -> Result<String> {
    let value = wide(value)?;
    let required = unsafe { ExpandEnvironmentStringsW(PCWSTR(value.as_ptr()), None) };
    ensure!(
        required > 0 && required <= 32768,
        "无法展开启动项的环境变量"
    );
    let mut buffer = vec![0u16; required as usize];
    let actual = unsafe { ExpandEnvironmentStringsW(PCWSTR(value.as_ptr()), Some(&mut buffer)) };
    ensure!(
        actual > 0 && actual <= required,
        "启动项环境变量在读取时发生变化，请刷新"
    );
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..end]))
}

fn read_startup_folder(id: &GUID, source: &str, entries: &mut Vec<StartupEntry>) -> Result<()> {
    let allocated = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
        .with_context(|| format!("无法查找{source}"))?;
    let decoded = unsafe { allocated.to_string() };
    unsafe { CoTaskMemFree(Some(allocated.0.cast::<c_void>())) };
    let path = PathBuf::from(decoded.with_context(|| format!("{source}路径无效"))?);
    if !path.exists() {
        return Ok(());
    }
    for item in std::fs::read_dir(&path).with_context(|| format!("无法读取{source}"))? {
        let item = item.with_context(|| format!("无法读取{source}中的文件"))?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case("desktop.ini") || !item.file_type()?.is_file() {
            continue;
        }
        use std::os::windows::fs::MetadataExt;
        let metadata = item.metadata()?;
        let origin = StartupOrigin::Folder {
            path: item.path(),
            machine: *id == FOLDERID_CommonStartup,
            size: metadata.file_size(),
            modified: metadata.last_write_time(),
            created: metadata.creation_time(),
        };
        let approval = read_approval(&origin, &name).unwrap_or(ApprovalSnapshot::Unreadable);
        entries.push(StartupEntry {
            name: name.clone(),
            command: item.path().to_string_lossy().into_owned(),
            source: source.to_owned(),
            enabled: approval_enabled(&approval),
            identity: StartupIdentity {
                name: name.clone(),
                origin,
                approval,
            },
        });
    }
    Ok(())
}

fn verified_process(
    pid: u32,
    expected_start_time: u64,
    access: PROCESS_ACCESS_RIGHTS,
) -> Result<OwnedHandle> {
    ensure!(pid != 0 && pid != 4, "禁止操作 Windows 系统核心进程");
    ensure!(
        pid != std::process::id(),
        "禁止操作 SystemMonitorX 自身进程"
    );
    ensure!(
        expected_start_time > 0,
        "缺少进程身份信息，请刷新进程列表后重试"
    );
    let handle = OwnedHandle(
        unsafe { OpenProcess(access | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .with_context(|| format!("无法访问进程 {pid}：进程可能已退出，或需要管理员权限"))?,
    );
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
        .context("无法验证进程启动时间")?;
    let windows_ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    let start_time = (windows_ticks / 10_000_000).saturating_sub(11_644_473_600);
    // New collectors supply the original FILETIME; retain Unix-seconds support
    // for existing callers. Raw FILETIME values cannot overlap Unix timestamps.
    let identity_matches = if expected_start_time >= 116_444_736_000_000_000 {
        windows_ticks == expected_start_time
    } else {
        start_time == expected_start_time
    };
    ensure!(
        identity_matches,
        "进程已发生变化（PID 被重新使用），请刷新后重试"
    );
    ensure!(
        exited.dwHighDateTime == 0 && exited.dwLowDateTime == 0,
        "该进程已经退出，请刷新后重试"
    );
    let mut critical = BOOL::default();
    unsafe { IsProcessCritical(handle.0, &mut critical) }
        .context("无法确认进程是否为系统关键进程，已取消操作")?;
    ensure!(
        !critical.as_bool(),
        "禁止操作 Windows 关键进程，以避免系统崩溃"
    );
    Ok(handle)
}

/// Terminates only the process represented by the supplied PID and start time.
pub fn terminate_process(pid: u32, expected_start_time: u64) -> Result<()> {
    let process = verified_process(pid, expected_start_time, PROCESS_TERMINATE)?;
    unsafe { TerminateProcess(process.0, 1) }
        .context("结束进程失败：可能受到系统保护或需要管理员权限")
}

pub fn set_priority(pid: u32, expected_start_time: u64, priority: Priority) -> Result<()> {
    let process = verified_process(pid, expected_start_time, PROCESS_SET_INFORMATION)?;
    let class = match priority {
        Priority::Idle => IDLE_PRIORITY_CLASS,
        Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        Priority::Normal => NORMAL_PRIORITY_CLASS,
        Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        Priority::High => HIGH_PRIORITY_CLASS,
    };
    unsafe { SetPriorityClass(process.0, class) }.context("设置优先级失败：可能需要管理员权限")
}

pub fn launch_task(command: &str) -> Result<()> {
    let command = command.trim();
    ensure!(!command.is_empty(), "请输入程序名称或完整命令");
    let mut command = wide(command)?;
    ensure!(command.len() <= 32767, "命令超过 Windows 支持的最大长度");
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .context("启动任务失败：请检查程序路径和参数（含空格的路径需要双引号）")?;
    let _process = OwnedHandle(info.hProcess);
    let _thread = OwnedHandle(info.hThread);
    Ok(())
}

pub fn open_file_location(path: &str) -> Result<()> {
    ensure!(
        !path.contains('\0') && !path.is_empty(),
        "进程文件路径无效或不可读取"
    );
    let path = PathBuf::from(path);
    ensure!(path.exists(), "程序文件已经不存在或无权访问");
    let explorer =
        PathBuf::from(std::env::var_os("SystemRoot").context("找不到 Windows 系统目录")?)
            .join("explorer.exe");
    let mut command = Command::new(explorer);
    if path.is_dir() {
        command.arg(&path);
    } else {
        command.arg(format!("/select,{}", path.display()));
    }
    command.spawn().context("无法打开文件资源管理器")?;
    Ok(())
}

pub fn is_elevated() -> Result<bool> {
    let mut raw_token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) }
        .context("无法读取当前进程权限")?;
    let token = OwnedHandle(raw_token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }
    .context("无法检查管理员权限")?;
    Ok(elevation.TokenIsElevated != 0)
}

fn approval_location(origin: &StartupOrigin) -> (HKEY, String) {
    let (machine, key) = match origin {
        StartupOrigin::Registry {
            machine, approval_key, ..
        } => (*machine, approval_key.clone()),
        StartupOrigin::Folder { machine, .. } => (*machine,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder".to_owned()),
    };
    (
        if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        },
        key,
    )
}

fn read_registry_value(
    root: HKEY,
    key: &str,
    view: REG_SAM_FLAGS,
    name: &str,
) -> Result<Option<(u32, Vec<u8>)>> {
    let key = wide(key)?;
    let mut handle = HKEY::default();
    let result = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(key.as_ptr()),
            None,
            KEY_READ | view,
            &mut handle,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    result.ok().context("无法读取注册表项")?;
    let handle = RegistryHandle(handle);
    let name = wide(name)?;
    let mut kind = registry::REG_VALUE_TYPE::default();
    let mut length = 0;
    let result = unsafe {
        registry::RegQueryValueExW(
            handle.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut length),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    result.ok().context("无法读取注册表值")?;
    ensure!(length <= 1024 * 1024, "注册表值过大");
    let mut bytes = vec![0; length as usize];
    unsafe {
        registry::RegQueryValueExW(
            handle.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(bytes.as_mut_ptr()),
            Some(&mut length),
        )
    }
    .ok()
    .context("注册表值读取时发生变化，请刷新")?;
    bytes.truncate(length as usize);
    Ok(Some((kind.0, bytes)))
}

fn read_approval(origin: &StartupOrigin, name: &str) -> Result<ApprovalSnapshot> {
    let (root, key) = approval_location(origin);
    Ok(
        match read_registry_value(root, &key, KEY_WOW64_64KEY, name)? {
            None => ApprovalSnapshot::Absent,
            Some((kind, bytes)) if kind == registry::REG_BINARY.0 => {
                ApprovalSnapshot::Binary(bytes)
            }
            Some(_) => ApprovalSnapshot::Unreadable,
        },
    )
}

fn approval_enabled(snapshot: &ApprovalSnapshot) -> Option<bool> {
    match snapshot {
        // A normal Run/StartupFolder entry with no approval override is enabled.
        ApprovalSnapshot::Absent => Some(true),
        // Explorer's format is undocumented; Electron's browser_win.cc also
        // uses 12-byte state 3 for disabled and recognizes state 2 as enabled:
        // https://github.com/electron/electron/blob/main/shell/browser/browser_win.cc
        // Only the observed 12-byte state 2/3
        // layout is supported; other formats/policy states stay unknown/read-only.
        ApprovalSnapshot::Binary(bytes) if bytes.len() == 12 => {
            match u32::from_le_bytes(bytes[..4].try_into().ok()?) {
                2 => Some(true),
                3 => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Changes only Explorer's approval record. Run commands and .lnk files remain
/// intact. Unknown/policy-owned approval formats are never overwritten.
pub fn set_startup_enabled(entry: &StartupEntry, enabled: bool) -> Result<()> {
    ensure!(
        entry.name == entry.identity.name,
        "启动项名称发生变化，请刷新"
    );
    match &entry.identity.origin {
        StartupOrigin::Registry {
            machine,
            view32,
            key,
            kind,
            bytes,
            ..
        } => {
            let current = read_registry_value(
                if *machine {
                    HKEY_LOCAL_MACHINE
                } else {
                    HKEY_CURRENT_USER
                },
                key,
                if *view32 {
                    KEY_WOW64_32KEY
                } else {
                    KEY_WOW64_64KEY
                },
                &entry.name,
            )?;
            ensure!(
                current
                    .as_ref()
                    .is_some_and(|(current_kind, current_bytes)| current_kind == kind
                        && current_bytes == bytes),
                "启动项命令已被修改或删除，请刷新后重试"
            );
        }
        StartupOrigin::Folder {
            path,
            size,
            modified,
            created,
            ..
        } => {
            use std::os::windows::fs::MetadataExt;
            let current = std::fs::metadata(path).context("启动文件已不存在或不可读取")?;
            ensure!(
                current.is_file()
                    && current.file_size() == *size
                    && current.last_write_time() == *modified
                    && current.creation_time() == *created,
                "启动文件发生变化，请刷新后重试"
            );
        }
    }
    let current = read_approval(&entry.identity.origin, &entry.name)?;
    ensure!(
        current == entry.identity.approval,
        "启动项启用状态已被其他程序修改，请刷新后重试"
    );
    let previous =
        approval_enabled(&current).context("启动项审批状态未知或受系统策略管理，不能安全修改")?;
    if previous == enabled {
        return Ok(());
    }
    let mut bytes = vec![0u8; 12];
    bytes[..4].copy_from_slice(&(if enabled { 2u32 } else { 3u32 }).to_le_bytes());
    if !enabled {
        let now = unsafe { GetSystemTimeAsFileTime() };
        let ticks = ((now.dwHighDateTime as u64) << 32) | now.dwLowDateTime as u64;
        bytes[4..].copy_from_slice(&ticks.to_le_bytes());
    }
    let (root, key) = approval_location(&entry.identity.origin);
    let key = wide(&key)?;
    let mut handle = HKEY::default();
    unsafe {
        registry::RegCreateKeyExW(
            root,
            PCWSTR(key.as_ptr()),
            None,
            PCWSTR::null(),
            registry::REG_OPTION_NON_VOLATILE,
            registry::KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &mut handle,
            None,
        )
    }
    .ok()
    .context("无法打开启动项审批记录：系统启动项可能需要管理员权限")?;
    let handle = RegistryHandle(handle);
    let name = wide(&entry.name)?;
    unsafe {
        registry::RegSetValueExW(
            handle.0,
            PCWSTR(name.as_ptr()),
            None,
            registry::REG_BINARY,
            Some(&bytes),
        )
    }
    .ok()
    .context("修改启动项状态失败：可能缺少权限")
}

#[derive(Clone, Copy, Debug)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

fn service_status(handle: SC_HANDLE) -> Result<scm::SERVICE_STATUS_PROCESS> {
    let mut status = scm::SERVICE_STATUS_PROCESS::default();
    let buffer = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut status as *mut scm::SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<scm::SERVICE_STATUS_PROCESS>(),
        )
    };
    let mut needed = 0;
    unsafe {
        scm::QueryServiceStatusEx(
            handle,
            scm::SC_STATUS_PROCESS_INFO,
            Some(buffer),
            &mut needed,
        )
    }
    .context("无法读取服务状态")?;
    Ok(status)
}

fn wait_for_service(handle: SC_HANDLE, desired: scm::SERVICE_STATUS_CURRENT_STATE) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = service_status(handle)?;
        if status.dwCurrentState == desired {
            return Ok(());
        }
        if status.dwCurrentState == SERVICE_STOPPED && desired == SERVICE_RUNNING {
            bail!(
                "服务启动失败：Win32 错误 {}，服务错误 {}",
                status.dwWin32ExitCode,
                status.dwServiceSpecificExitCode
            );
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "等待服务状态超时；操作可能仍在进行，请刷新确认"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Blocking SCM operation; call from the UI's background worker after explicit
/// user confirmation. No dependent services are force-stopped.
pub fn control_service(expected: &ServiceInfo, action: ServiceAction) -> Result<()> {
    let configuration = expected
        .configuration
        .as_ref()
        .context("服务配置信息未知，请刷新或使用管理员权限读取后再操作")?;
    ensure!(!expected.name.is_empty(), "服务名称为空");
    ensure!(
        configuration.service_type & SERVICE_DRIVER.0 == 0,
        "不直接控制内核驱动服务，请使用 Windows 设备管理工具"
    );
    let manager = ServiceHandle(
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), scm::SC_MANAGER_CONNECT) }
            .context("无法连接服务管理器")?,
    );
    ensure!(
        service_configuration(manager.0, &expected.name)? == *configuration,
        "服务配置已经变化，请刷新后重试"
    );
    let name = wide(&expected.name)?;
    let desired_access = scm::SERVICE_QUERY_STATUS
        | scm::SERVICE_QUERY_CONFIG
        | match action {
            ServiceAction::Start => scm::SERVICE_START,
            ServiceAction::Stop => scm::SERVICE_STOP,
            ServiceAction::Restart => scm::SERVICE_START | scm::SERVICE_STOP,
        };
    let service = ServiceHandle(
        unsafe { OpenServiceW(manager.0, PCWSTR(name.as_ptr()), desired_access) }
            .context("无法管理服务：可能需要管理员权限或服务受到系统保护")?,
    );
    // Verify config again through the very same handle used for mutation.
    let mut needed = 0;
    let mut storage = vec![0usize; 8192 / size_of::<usize>()];
    let config_ptr = storage.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    unsafe { QueryServiceConfigW(service.0, Some(config_ptr), 8192, &mut needed) }
        .context("无法验证服务配置")?;
    let actual = unsafe { &*config_ptr };
    ensure!(
        unsafe { actual.lpBinaryPathName.to_string()? } == configuration.binary_path
            && unsafe { actual.lpServiceStartName.to_string()? } == configuration.account
            && actual.dwStartType.0 == configuration.start_type_code
            && actual.dwServiceType.0 == configuration.service_type,
        "服务身份或配置已被替换，请刷新后重试"
    );
    let status = service_status(service.0)?;
    ensure!(
        status.dwProcessId == expected.pid,
        "服务进程已经变化，请刷新后重试"
    );
    ensure!(
        matches!(
            status.dwCurrentState,
            SERVICE_RUNNING | SERVICE_STOPPED | SERVICE_PAUSED
        ),
        "服务正在切换状态，请等待后刷新"
    );
    if matches!(action, ServiceAction::Stop | ServiceAction::Restart)
        && status.dwCurrentState != SERVICE_STOPPED
    {
        ensure!(
            status.dwControlsAccepted & scm::SERVICE_ACCEPT_STOP != 0,
            "服务当前不允许停止"
        );
        let mut result = scm::SERVICE_STATUS::default();
        unsafe { scm::ControlService(service.0, scm::SERVICE_CONTROL_STOP, &mut result) }
            .context("停止服务失败：请检查权限、依赖服务或系统保护")?;
        wait_for_service(service.0, SERVICE_STOPPED)?;
    }
    if matches!(action, ServiceAction::Start | ServiceAction::Restart)
        && service_status(service.0)?.dwCurrentState != SERVICE_RUNNING
    {
        unsafe { scm::StartServiceW(service.0, None) }
            .context("启动服务失败：可能已禁用、缺少权限或依赖服务无法启动")?;
        wait_for_service(service.0, SERVICE_RUNNING)?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ProcessControls {
    pub affinity_mask: usize,
    pub system_affinity_mask: usize,
    pub processor_group: u16,
    pub priority_class: u32,
    pub efficiency_mode: bool,
    pub power_control_mask: u32,
    pub power_state_mask: u32,
}

fn process_affinity(handle: HANDLE) -> Result<(usize, usize, u16)> {
    let mut groups = [0u16; 64];
    let mut group_count = groups.len() as u16;
    unsafe { threading::GetProcessGroupAffinity(handle, &mut group_count, groups.as_mut_ptr()) }
        .ok()
        .context("无法读取 CPU 处理器组")?;
    ensure!(
        group_count == 1,
        "进程跨越多个处理器组，不能使用单组 CPU 亲和性设置"
    );
    let (mut process, mut system) = (0usize, 0usize);
    unsafe { threading::GetProcessAffinityMask(handle, &mut process, &mut system) }
        .context("无法读取 CPU 亲和性")?;
    ensure!(process != 0 && system != 0, "CPU 亲和性状态不明确");
    Ok((process, system, groups[0]))
}

fn power_state(handle: HANDLE) -> Result<threading::PROCESS_POWER_THROTTLING_STATE> {
    let mut state = threading::PROCESS_POWER_THROTTLING_STATE {
        Version: threading::PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ..Default::default()
    };
    unsafe {
        threading::GetProcessInformation(
            handle,
            threading::ProcessPowerThrottling,
            (&mut state as *mut threading::PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<threading::PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .context("无法读取效率模式；不会将未知状态当成已启用")?;
    Ok(state)
}

pub fn process_controls(pid: u32, expected_start_time: u64) -> Result<ProcessControls> {
    let process = verified_process(pid, expected_start_time, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let (affinity_mask, system_affinity_mask, processor_group) = process_affinity(process.0)?;
    let state = power_state(process.0)?;
    let priority = unsafe { threading::GetPriorityClass(process.0) };
    ensure!(priority != 0, "无法读取进程优先级");
    let flag = threading::PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
    Ok(ProcessControls {
        affinity_mask,
        system_affinity_mask,
        processor_group,
        priority_class: priority,
        efficiency_mode: state.ControlMask & state.StateMask & flag != 0,
        power_control_mask: state.ControlMask,
        power_state_mask: state.StateMask,
    })
}

pub fn set_affinity(pid: u32, expected_start_time: u64, mask: usize) -> Result<()> {
    let process = verified_process(pid, expected_start_time, PROCESS_SET_INFORMATION)?;
    let (_, available, _) = process_affinity(process.0)?;
    ensure!(
        mask != 0 && mask & !available == 0,
        "至少选择一个当前处理器组内可用的 CPU"
    );
    unsafe { threading::SetProcessAffinityMask(process.0, mask) }
        .context("设置 CPU 亲和性失败：可能缺少权限")
}

/// Efficiency mode combines EcoQoS with idle scheduling priority, as Windows
/// Task Manager does. Read failures leave the process unchanged.
/// https://devblogs.microsoft.com/performance-diagnostics/reduce-process-interference-with-task-manager-efficiency-mode/
pub fn set_efficiency_mode(pid: u32, expected_start_time: u64, enabled: bool) -> Result<()> {
    let process = verified_process(pid, expected_start_time, PROCESS_SET_INFORMATION)?;
    let previous = power_state(process.0)?;
    let priority = unsafe { threading::GetPriorityClass(process.0) };
    ensure!(priority != 0, "无法读取进程优先级，未修改效率模式");
    let flag = threading::PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
    let next = threading::PROCESS_POWER_THROTTLING_STATE {
        Version: previous.Version,
        ControlMask: previous.ControlMask | flag,
        StateMask: if enabled {
            previous.StateMask | flag
        } else {
            previous.StateMask & !flag
        },
    };
    unsafe {
        threading::SetProcessInformation(
            process.0,
            threading::ProcessPowerThrottling,
            (&next as *const threading::PROCESS_POWER_THROTTLING_STATE).cast(),
            size_of::<threading::PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .context("设置 EcoQoS 失败：系统可能不支持或缺少权限")?;
    let next_priority = if enabled {
        IDLE_PRIORITY_CLASS
    } else if priority == IDLE_PRIORITY_CLASS.0 {
        NORMAL_PRIORITY_CLASS
    } else {
        threading::PROCESS_CREATION_FLAGS(priority)
    };
    if let Err(error) = unsafe { SetPriorityClass(process.0, next_priority) } {
        let rollback = unsafe {
            threading::SetProcessInformation(
                process.0,
                threading::ProcessPowerThrottling,
                (&previous as *const threading::PROCESS_POWER_THROTTLING_STATE).cast(),
                size_of::<threading::PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if let Err(rollback_error) = rollback {
            bail!(
                "优先级设置失败（{error}），恢复 EcoQoS 也失败（{rollback_error}）；请刷新检查当前状态"
            );
        }
        return Err(error).context("设置效率模式优先级失败，EcoQoS 已恢复");
    }
    Ok(())
}

fn creation_ticks(handle: HANDLE) -> Result<u64> {
    let (mut creation, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
        .context("无法验证子进程创建时间")?;
    ensure!(
        exit.dwLowDateTime == 0 && exit.dwHighDateTime == 0,
        "子进程已经退出"
    );
    Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

fn process_parent_snapshot() -> Result<Vec<(u32, u32)>> {
    let snapshot = OwnedHandle(
        unsafe { toolhelp::CreateToolhelp32Snapshot(toolhelp::TH32CS_SNAPPROCESS, 0) }
            .context("无法获取进程树")?,
    );
    let mut entry = toolhelp::PROCESSENTRY32W {
        dwSize: size_of::<toolhelp::PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { toolhelp::Process32FirstW(snapshot.0, &mut entry) }.context("无法读取进程树")?;
    let mut entries = Vec::new();
    loop {
        entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
        match unsafe { toolhelp::Process32NextW(snapshot.0, &mut entry) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(error).context("读取进程树失败"),
        }
    }
    Ok(entries)
}

/// Captures and protects every target handle before ending any process. Newly
/// created descendants after this snapshot are not included.
pub fn terminate_process_tree(pid: u32, expected_start_time: u64) -> Result<Vec<u32>> {
    let root = verified_process(pid, expected_start_time, PROCESS_TERMINATE)?;
    let root_ticks = creation_ticks(root.0)?;
    let links = process_parent_snapshot()?;
    let mut targets = vec![(pid, root_ticks, root)];
    let mut cursor = 0;
    while cursor < targets.len() {
        let (parent, parent_ticks, _) = &targets[cursor];
        let (parent, parent_ticks) = (*parent, *parent_ticks);
        for &(child, recorded_parent) in &links {
            if recorded_parent != parent || targets.iter().any(|(id, _, _)| *id == child) {
                continue;
            }
            ensure!(
                child != 0 && child != 4 && child != std::process::id(),
                "进程树包含受保护进程，未结束任何进程"
            );
            let handle = OwnedHandle(
                unsafe {
                    OpenProcess(
                        PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                        false,
                        child,
                    )
                }
                .with_context(|| format!("无法验证子进程 {child}，未结束任何进程"))?,
            );
            let ticks = creation_ticks(handle.0)?;
            // Parent PID reuse can make an unrelated older process look like a
            // descendant. Holding all handles also prevents reuse after validation.
            if ticks < parent_ticks {
                continue;
            }
            let mut critical = BOOL::default();
            unsafe { IsProcessCritical(handle.0, &mut critical) }
                .with_context(|| format!("无法验证子进程 {child} 是否为关键进程"))?;
            ensure!(
                !critical.as_bool(),
                "进程树包含关键进程 {child}，未结束任何进程"
            );
            targets.push((child, ticks, handle));
        }
        cursor += 1;
    }
    // Recheck the parent relationship after opening the handles: a child PID
    // recycled between the first snapshot and OpenProcess must not be targeted.
    let current_links = process_parent_snapshot()?;
    for (child, _, _) in targets.iter().skip(1) {
        let expected_parent = links
            .iter()
            .find(|(id, _)| id == child)
            .map(|(_, parent)| *parent);
        ensure!(
            current_links
                .iter()
                .find(|(id, _)| id == child)
                .map(|(_, parent)| *parent)
                == expected_parent,
            "子进程 {child} 的身份关系已变化，未结束任何进程"
        );
    }
    let mut terminated = Vec::new();
    for (id, _, handle) in targets.into_iter().rev() {
        unsafe { TerminateProcess(handle.0, 1) }
            .with_context(|| format!("结束进程 {id} 失败；已经结束的进程 PID：{terminated:?}"))?;
        terminated.push(id);
    }
    Ok(terminated)
}

#[derive(Clone, Copy, Debug)]
pub enum UserSessionAction {
    Disconnect,
    Logoff,
}

struct SessionBuffer(PWSTR);
impl Drop for SessionBuffer {
    fn drop(&mut self) {
        unsafe {
            wts::WTSFreeMemory(self.0.0.cast());
        }
    }
}

/// The logon timestamp prevents applying an old UI selection to a reused ID.
pub fn control_user_session(
    id: u32,
    expected_user: &str,
    expected_domain: &str,
    expected_logon_time: i64,
    action: UserSessionAction,
) -> Result<()> {
    ensure!(
        id != 0 && !expected_user.is_empty() && expected_logon_time > 0,
        "会话身份不完整，不能执行断开或注销"
    );
    let mut buffer = PWSTR::null();
    let mut bytes = 0;
    unsafe {
        wts::WTSQuerySessionInformationW(None, id, wts::WTSSessionInfo, &mut buffer, &mut bytes)
    }
    .context("无法验证用户会话：可能需要管理员权限")?;
    let buffer = SessionBuffer(buffer);
    ensure!(
        !buffer.0.is_null() && bytes as usize >= size_of::<wts::WTSINFOW>(),
        "用户会话信息不完整"
    );
    let current = unsafe { &*buffer.0.0.cast::<wts::WTSINFOW>() };
    let decode = |value: &[u16]| {
        String::from_utf16_lossy(
            &value[..value
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(value.len())],
        )
    };
    ensure!(
        current.SessionId == id
            && decode(&current.UserName) == expected_user
            && decode(&current.Domain) == expected_domain
            && current.LogonTime == expected_logon_time,
        "用户会话已改变，请刷新后重试"
    );
    match action {
        UserSessionAction::Disconnect => unsafe { wts::WTSDisconnectSession(None, id, false) }
            .context("断开用户会话失败：请检查权限或会话状态"),
        UserSessionAction::Logoff => unsafe { wts::WTSLogoffSession(None, id, false) }
            .context("注销用户会话失败：请检查权限或会话状态"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_unknown_is_never_treated_as_enabled() {
        assert_eq!(approval_enabled(&ApprovalSnapshot::Absent), Some(true));
        for (word, expected) in [(2u32, Some(true)), (3, Some(false)), (0, None), (6, None)] {
            let mut data = vec![0u8; 12];
            data[..4].copy_from_slice(&word.to_le_bytes());
            assert_eq!(approval_enabled(&ApprovalSnapshot::Binary(data)), expected);
        }
        assert_eq!(approval_enabled(&ApprovalSnapshot::Binary(vec![2])), None);
        assert_eq!(approval_enabled(&ApprovalSnapshot::Unreadable), None);
    }

    struct RegistryFixture(String);
    impl Drop for RegistryFixture {
        fn drop(&mut self) {
            // Only this test's unique HKCU subtree is ever removed.
            assert!(
                self.0
                    .starts_with("Software\\SystemMonitorX\\Tests\\startup-")
            );
            if let Ok(key) = wide(&self.0) {
                unsafe {
                    let _ = registry::RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()));
                }
            }
        }
    }

    fn fixture_value(
        key: &str,
        name: &str,
        kind: registry::REG_VALUE_TYPE,
        bytes: &[u8],
    ) -> Result<()> {
        let path = wide(key)?;
        let name = wide(name)?;
        let mut handle = HKEY::default();
        unsafe {
            registry::RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                registry::REG_OPTION_NON_VOLATILE,
                registry::KEY_SET_VALUE | KEY_WOW64_64KEY,
                None,
                &mut handle,
                None,
            )
        }
        .ok()?;
        let handle = RegistryHandle(handle);
        unsafe {
            registry::RegSetValueExW(handle.0, PCWSTR(name.as_ptr()), None, kind, Some(bytes))
        }
        .ok()?;
        Ok(())
    }

    #[test]
    fn startup_toggle_preserves_command_and_refuses_stale_identity() -> Result<()> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let fixture = RegistryFixture(format!(
            "Software\\SystemMonitorX\\Tests\\startup-{}-{unique}",
            std::process::id()
        ));
        let run = format!("{}\\Run", fixture.0);
        let approved = format!("{}\\StartupApproved", fixture.0);
        let command = "\"C:\\test only\\fixture.exe\" --preserve-command";
        let command_bytes: Vec<u8> = wide(command)?
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        fixture_value(&run, "Owned fixture", REG_SZ, &command_bytes)?;
        let mut entry = StartupEntry {
            name: "Owned fixture".to_owned(),
            command: command.to_owned(),
            source: "HKCU dedicated test fixture".to_owned(),
            enabled: Some(true),
            identity: StartupIdentity {
                name: "Owned fixture".to_owned(),
                approval: ApprovalSnapshot::Absent,
                origin: StartupOrigin::Registry {
                    machine: false,
                    view32: false,
                    key: run.clone(),
                    approval_key: approved.clone(),
                    kind: REG_SZ.0,
                    bytes: command_bytes.clone(),
                },
            },
        };
        set_startup_enabled(&entry, false)?;
        assert_eq!(
            read_registry_value(HKEY_CURRENT_USER, &run, KEY_WOW64_64KEY, &entry.name)?,
            Some((REG_SZ.0, command_bytes.clone()))
        );
        let disabled = read_approval(&entry.identity.origin, &entry.name)?;
        assert_eq!(approval_enabled(&disabled), Some(false));
        if let ApprovalSnapshot::Binary(data) = &disabled {
            assert!(data[4..].iter().any(|byte| *byte != 0));
        }
        assert!(
            set_startup_enabled(&entry, true).is_err(),
            "stale approval must fail before writing"
        );
        entry.identity.approval = disabled;
        set_startup_enabled(&entry, true)?;
        entry.identity.approval = read_approval(&entry.identity.origin, &entry.name)?;
        assert_eq!(approval_enabled(&entry.identity.approval), Some(true));
        fixture_value(&run, &entry.name, REG_SZ, &[0, 0])?;
        assert!(
            set_startup_enabled(&entry, false).is_err(),
            "changed command must fail before writing"
        );
        fixture_value(&run, &entry.name, REG_SZ, &command_bytes)?;
        fixture_value(
            &approved,
            &entry.name,
            registry::REG_BINARY,
            &[6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )?;
        entry.identity.approval = read_approval(&entry.identity.origin, &entry.name)?;
        assert!(
            set_startup_enabled(&entry, true).is_err(),
            "unknown approval must fail closed"
        );
        Ok(())
    }

    #[test]
    fn management_identity_guards_fail_before_mutation() {
        assert!(set_affinity(0, 1, 1).is_err());
        assert!(set_efficiency_mode(4, 1, true).is_err());
        assert!(terminate_process_tree(std::process::id(), 1).is_err());
        assert!(control_user_session(0, "fixture", "", 1, UserSessionAction::Logoff).is_err());
        let mut service = ServiceInfo {
            name: "Owned fixture".to_owned(),
            display_name: String::new(),
            status: String::new(),
            pid: 0,
            start_type: String::new(),
            configuration: None,
        };
        assert!(control_service(&service, ServiceAction::Stop).is_err());
        service.configuration = Some(ServiceConfiguration {
            binary_path: String::new(),
            account: String::new(),
            start_type_code: 3,
            service_type: SERVICE_DRIVER.0,
        });
        assert!(control_service(&service, ServiceAction::Start).is_err());
    }
}

//! Windows management operations. Resource handles are kept alive between identity
//! verification and mutation so that a recycled PID cannot target another process.

use std::{ffi::c_void, mem::size_of, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail, ensure};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
            ERROR_NO_MORE_ITEMS, FILETIME, HANDLE,
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
}

#[derive(Clone, Debug)]
pub struct StartupEntry {
    pub name: String,
    pub command: String,
    pub source: String,
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
            let start_type =
                service_start_type(manager.0, &name).unwrap_or_else(|_| "无法读取配置".to_owned());
            services.push(ServiceInfo {
                name,
                display_name,
                status,
                pid: entry.ServiceStatusProcess.dwProcessId,
                start_type,
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

fn service_start_type(manager: SC_HANDLE, name: &str) -> Result<String> {
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
    Ok(match unsafe { (*config).dwStartType } {
        SERVICE_BOOT_START => "引导启动",
        SERVICE_SYSTEM_START => "系统启动",
        SERVICE_AUTO_START => "自动",
        SERVICE_DEMAND_START => "手动",
        SERVICE_DISABLED => "已禁用",
        _ => "未知启动类型",
    }
    .to_owned())
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
        entries.push(StartupEntry {
            name: String::from_utf16_lossy(&name[..name_len]),
            command,
            source: source.to_owned(),
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
        entries.push(StartupEntry {
            name,
            command: item.path().to_string_lossy().into_owned(),
            source: source.to_owned(),
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
    ensure!(
        start_time == expected_start_time,
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

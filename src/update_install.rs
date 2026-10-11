//! User-requested MSI hand-off. The running GUI never installs while holding
//! its own executable/runtime DLLs open: a copied helper waits for its exact
//! process identity to exit normally, runs Windows Installer, records the
//! result, and reopens the application after success, cancellation or failure.
//!
//! Windows Installer flags and exit codes follow the Microsoft documentation:
//! https://learn.microsoft.com/windows-server/administration/windows-commands/msiexec
//! https://learn.microsoft.com/windows/win32/msi/error-codes
use crate::updater::{ReleaseInfo, verify_file};
use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
    path::{Component, Path, PathBuf, Prefix},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND,
            ERROR_PATH_NOT_FOUND, FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::FILE_SHARE_READ,
        System::{
            Registry::{
                HKEY_CURRENT_USER, RRF_NOEXPAND, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
            },
            SystemInformation::GetSystemDirectoryW,
            Threading::{
                CREATE_NO_WINDOW, GetCurrentProcess, GetProcessTimes, OpenProcess,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
            },
        },
    },
    core::{GUID, w},
};

const APP_NAME: &str = "SystemMonitorX.exe";
const HELPER_NAME: &str = "SystemMonitorXUpdate.exe";
const JOB_NAME: &str = "update-job.json";
const RESULT_NAME: &str = "install-result.json";
const READY_NAME: &str = "helper-ready.json";
const LOG_NAME: &str = "install.log";
const LAST_RESULT_NAME: &str = "last-install-result.json";
const MAX_JSON_BYTES: u64 = 128 * 1024;
const MAX_MSI_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RUNTIME_FILE_BYTES: u64 = 32 * 1024 * 1024;
const JOB_VERSION: u32 = 1;

/// Owns only the Child returned by this preparation's spawn. Failed hand-offs
/// must not leave a delayed helper waiting to install after the GUI closes.
struct PendingHelper {
    child: Option<Child>,
}

impl PendingHelper {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("pending helper owns its child")
    }

    /// Successful readiness transfers the still-running helper to installation.
    /// Dropping std::process::Child only closes our handle; it does not kill it.
    fn detach(mut self) -> Child {
        self.child.take().expect("pending helper owns its child")
    }
}

impl Drop for PendingHelper {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        // Never look up or terminate a process by PID. Child keeps the exact
        // process handle from spawn. A failed kill must not lead to an unbounded
        // wait on a live process; already-exited children can safely be reaped.
        if !matches!(child.try_wait(), Ok(Some(_))) && child.kill().is_err() {
            return;
        }
        let _ = child.wait();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateJob {
    pub format_version: u32,
    pub job_id: String,
    pub job_directory: PathBuf,
    pub installer_path: PathBuf,
    pub release: ReleaseInfo,
    pub old_app_pid: u32,
    /// Raw GetProcessTimes creation FILETIME, never rounded Unix seconds.
    pub old_app_creation_ticks: u64,
    pub app_executable: PathBuf,
    pub helper_executable: PathBuf,
    pub result_path: PathBuf,
    pub last_result_path: PathBuf,
    pub log_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallStatus {
    Succeeded,
    RebootRequired,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallResult {
    pub job_id: String,
    pub version: String,
    pub status: InstallStatus,
    pub exit_code: Option<u32>,
    pub message: String,
    pub log_path: PathBuf,
    pub installer_path: PathBuf,
    pub completed_at_unix_secs: u64,
    pub application_restarted: bool,
    #[serde(default)]
    pub restarted_executable: Option<PathBuf>,
    pub restart_error: Option<String>,
}

impl InstallResult {
    pub fn succeeded(&self) -> bool {
        matches!(
            self.status,
            InstallStatus::Succeeded | InstallStatus::RebootRequired
        )
    }
}

fn updates_directory() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("Windows 用户应用数据目录不可用")?;
    let path = PathBuf::from(local);
    ensure!(path.is_absolute(), "Windows 用户应用数据目录不是绝对路径");
    Ok(path.join("SystemMonitorX").join("updates"))
}

pub fn last_result_path() -> Result<PathBuf> {
    Ok(updates_directory()?.join(LAST_RESULT_NAME))
}

pub fn read_last_result() -> Result<Option<InstallResult>> {
    let path = last_result_path()?;
    match read_bounded_json(&path) {
        Ok(result) => Ok(Some(result)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.context("无法读取上次安装结果")),
    }
}

fn unique_id() -> Result<String> {
    Ok(format!("{:032x}", GUID::new()?.to_u128()))
}

fn valid_job_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn has_filename(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn validate_release(release: &ReleaseInfo) -> Result<String> {
    let version = Version::parse(&release.version).context("安装版本号无效")?;
    ensure!(
        version.pre.is_empty()
            && version.build.is_empty()
            && release.version == version.to_string(),
        "仅支持稳定发布安装包"
    );
    let name = format!("SystemMonitorX-{version}-x64.msi");
    ensure!(release.installer_name == name, "安装包名称与版本不符");
    let base = "https://github.com/neko233-com/SystemMonitorX/releases";
    ensure!(
        release.page_url == format!("{base}/tag/v{version}"),
        "安装版本地址不属于本项目"
    );
    ensure!(
        release.installer_url == format!("{base}/download/v{version}/{name}"),
        "安装包地址不属于本项目"
    );
    ensure!(
        release.installer_bytes > 0 && release.installer_bytes <= MAX_MSI_BYTES,
        "安装包大小无效"
    );
    ensure!(
        release.installer_sha256.len() == 64
            && release
                .installer_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "安装包 SHA-256 格式无效"
    );
    Ok(version.to_string())
}

fn runtime_filename(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".dll")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
        && ["vcruntime", "msvcp", "concrt", "vccorlib", "vcomp", "vcamp"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn creation_ticks(handle: HANDLE) -> Result<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
        .context("无法核验进程创建时间")?;
    Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

fn copy_bundled_helper(app_directory: &Path, destination: &Path) -> Result<PathBuf> {
    let source = app_directory
        .join(HELPER_NAME)
        .canonicalize()
        .context("缺少更新安装助手，请安装完整的 SystemMonitorX 发布包")?;
    ensure!(
        source.parent() == Some(app_directory),
        "安装助手必须位于当前应用目录"
    );
    ensure!(
        source.is_file() && has_filename(&source, HELPER_NAME),
        "当前目录中的安装助手无效"
    );
    let target = destination.join(HELPER_NAME);
    fs::copy(&source, &target).context("无法复制安装助手到更新缓存")?;
    let mut copied_count = 0;
    let mut copied_bytes = 0u64;
    for entry in fs::read_dir(app_directory)? {
        let entry = entry?;
        let filename = entry.file_name();
        let Some(name) = filename.to_str() else {
            continue;
        };
        if !runtime_filename(name) {
            continue;
        }
        let source = entry.path().canonicalize()?;
        ensure!(
            source.parent() == Some(app_directory) && source.is_file(),
            "随包运行库不属于当前应用目录"
        );
        let bytes = source.metadata()?.len();
        copied_count += 1;
        copied_bytes = copied_bytes
            .checked_add(bytes)
            .context("随包运行库大小溢出")?;
        ensure!(
            bytes <= MAX_RUNTIME_FILE_BYTES
                && copied_count <= 64
                && copied_bytes <= 128 * 1024 * 1024,
            "随包运行库大小超出限制"
        );
        fs::copy(source, destination.join(filename)).context("无法复制随包运行库")?;
    }
    target.canonicalize().context("无法解析缓存安装助手路径")
}

/// Blocking preparation must run on the user's install-click background task.
/// A successful return means the helper has started; the caller then quits its
/// own GUI normally. This function never terminates an existing process.
pub fn prepare_and_launch(msi: &Path, release: &ReleaseInfo) -> Result<()> {
    let version = validate_release(release)?;
    let app = std::env::current_exe()?.canonicalize()?;
    ensure!(
        has_filename(&app, APP_NAME),
        "更新只能由 SystemMonitorX.exe 发起"
    );
    let app_directory = app.parent().context("当前应用目录不可用")?;
    let root = updates_directory()?
        .canonicalize()
        .context("更新缓存目录不存在，请先下载安装包")?;
    let installer = msi.canonicalize().context("已下载的安装包不存在")?;
    let expected_parent = root
        .join(&version)
        .canonicalize()
        .context("安装版本缓存目录不存在")?;
    ensure!(
        expected_parent.parent() == Some(root.as_path())
            && installer.parent() == Some(expected_parent.as_path())
            && has_filename(&installer, &release.installer_name),
        "安装包不在本版本的应用更新缓存内"
    );
    verify_file(&installer, release)?;
    let job_id = unique_id()?;
    let directory = root.join(format!("helper-{job_id}"));
    fs::create_dir(&directory).context("无法创建独立安装助手缓存目录")?;
    let directory = directory.canonicalize()?;
    ensure!(
        directory.parent() == Some(root.as_path()),
        "安装助手缓存目录无效"
    );
    let log_path = directory.join(LOG_NAME);
    validate_installer_boundary(&installer, &log_path, &directory)?;
    // Reject paths that msiexec cannot represent before spawning/acknowledging
    // a helper, so a preparation error keeps the current GUI open.
    let _ = installer_command(&system_msiexec()?, &installer, &log_path, &directory)?;
    let helper = copy_bundled_helper(app_directory, &directory)?;
    let job = UpdateJob {
        format_version: JOB_VERSION,
        job_id,
        job_directory: directory.clone(),
        installer_path: installer,
        release: release.clone(),
        old_app_pid: std::process::id(),
        old_app_creation_ticks: creation_ticks(unsafe { GetCurrentProcess() })?,
        app_executable: app,
        helper_executable: helper.clone(),
        result_path: directory.join(RESULT_NAME),
        last_result_path: root.join(LAST_RESULT_NAME),
        log_path,
    };
    let path = directory.join(JOB_NAME);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(&job)?)?;
    file.sync_all()?;
    drop(file);
    let child = Command::new(helper)
        .arg("--job")
        .arg(&path)
        .current_dir(&directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .context("无法启动更新安装助手；请确认正在使用带完整运行库的发布包")?;
    let mut pending_helper = PendingHelper::new(child);
    let child = pending_helper.child_mut();
    // Wait only on this background preparation thread. The app must not quit
    // until the copied helper has verified the package and obtained the global
    // install lock. In particular, missing runtime DLLs or another app's update
    // cannot leave the user with a prematurely closed application window.
    // SAFETY: Child owns a live process handle for this exact spawned helper.
    let expected_helper_creation_ticks = creation_ticks(HANDLE(child.as_raw_handle()))?;
    let ready_path = directory.join(READY_NAME);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if let Some(status) = child.try_wait()? {
            let message = read_bounded_json::<InstallResult>(&job.result_path)
                .map(|result| result.message)
                .unwrap_or_else(|_| format!("安装助手未就绪（{status}）；请使用完整发布包"));
            bail!("{message}");
        }
        if let Ok(ready) = read_bounded_json::<HelperReady>(&ready_path) {
            ensure!(
                ready.job_id == job.job_id
                    && ready.helper_pid == child.id()
                    && ready.helper_creation_ticks == expected_helper_creation_ticks,
                "安装助手就绪身份无效"
            );
            drop(pending_helper.detach());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("安装助手尚未确认就绪，当前窗口保持打开；请稍后重试")
}

#[derive(Serialize, Deserialize)]
struct HelperReady {
    job_id: String,
    helper_pid: u32,
    helper_creation_ticks: u64,
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_JSON_BYTES,
        "安装任务/结果文件过大"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_JSON_BYTES,
        "安装任务/结果文件过大"
    );
    serde_json::from_slice(&bytes).context("安装任务/结果 JSON 无效")
}

fn validate_job_paths(job_path: &Path, job: &UpdateJob, root: &Path) -> Result<()> {
    ensure!(
        job.format_version == JOB_VERSION && valid_job_id(&job.job_id),
        "安装任务格式/标识无效"
    );
    let expected_directory = root.join(format!("helper-{}", job.job_id));
    ensure!(
        job_path == expected_directory.join(JOB_NAME) && job.job_directory == expected_directory,
        "安装任务不在独立应用更新缓存内"
    );
    ensure!(
        job.helper_executable == expected_directory.join(HELPER_NAME),
        "安装助手名称/目录无效"
    );
    ensure!(
        job.result_path == expected_directory.join(RESULT_NAME)
            && job.log_path == expected_directory.join(LOG_NAME)
            && job.last_result_path == root.join(LAST_RESULT_NAME),
        "安装日志/结果路径无效"
    );
    let version = validate_release(&job.release)?;
    ensure!(
        job.installer_path == root.join(version).join(&job.release.installer_name),
        "安装包不在本版本更新缓存内"
    );
    ensure!(
        job.app_executable.is_absolute() && has_filename(&job.app_executable, APP_NAME),
        "重启应用必须是绝对 SystemMonitorX.exe 路径"
    );
    ensure!(
        job.old_app_pid != 0 && job.old_app_creation_ticks != 0,
        "原应用身份无效"
    );
    Ok(())
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OriginalProcessExit {
    AlreadyExited,
    Exited,
    PidReused,
}

fn wait_for_exact_process(
    pid: u32,
    expected_ticks: u64,
    timeout: Duration,
) -> Result<OriginalProcessExit> {
    let handle = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    } {
        Ok(handle) => OwnedHandle(handle),
        Err(error)
            if error.code() == ERROR_INVALID_PARAMETER.to_hresult()
                || error.code() == ERROR_NOT_FOUND.to_hresult() =>
        {
            return Ok(OriginalProcessExit::AlreadyExited);
        }
        Err(error) => return Err(error.into()),
    };
    if creation_ticks(handle.0)? != expected_ticks {
        return Ok(OriginalProcessExit::PidReused);
    }
    match unsafe { WaitForSingleObject(handle.0, timeout.as_millis().min(u32::MAX as u128) as u32) }
    {
        WAIT_OBJECT_0 => Ok(OriginalProcessExit::Exited),
        WAIT_TIMEOUT => bail!("旧应用尚未正常退出（等待 30 秒），本次未安装；请正常关闭窗口后重试"),
        WAIT_FAILED => Err(std::io::Error::last_os_error()).context("等待原应用退出失败"),
        other => bail!("等待原应用退出返回未知状态 {}", other.0),
    }
}

fn system_msiexec() -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32_768];
    let count = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    ensure!(
        count > 0 && count < buffer.len(),
        "无法读取 Windows 系统目录"
    );
    use std::os::windows::ffi::OsStringExt;
    let path = PathBuf::from(OsString::from_wide(&buffer[..count])).join("msiexec.exe");
    ensure!(
        path.is_absolute() && path.is_file(),
        "Windows Installer 系统程序不存在"
    );
    path.canonicalize()
        .context("Windows Installer 系统路径无效")
}

/// Windows Installer's source-directory handling expects standard DOS/UNC
/// paths. Keep canonical verbatim paths for validation and file guards, and
/// translate only at this process boundary without lossy string replacement.
/// https://doc.rust-lang.org/std/path/enum.Prefix.html
/// https://learn.microsoft.com/windows/win32/fileio/naming-a-file
fn windows_installer_path(path: &Path) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    ensure!(
        path.is_absolute(),
        "Windows Installer 路径必须是绝对 DOS/UNC 路径"
    );
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        bail!("Windows Installer 路径缺少 DOS/UNC 前缀");
    };
    let mut normal = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
            PathBuf::from(format!("{}:\\", char::from(drive)))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            ensure!(
                !server.is_empty() && !share.is_empty(),
                "Windows Installer UNC 服务器/共享名为空"
            );
            let mut value = OsString::from(r"\\");
            value.push(server);
            value.push(r"\");
            value.push(share);
            value.push(r"\");
            PathBuf::from(value)
        }
        Prefix::DeviceNS(_) | Prefix::Verbatim(_) => {
            bail!("Windows Installer 不支持设备或原始命名空间路径");
        }
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let units: Vec<_> = name.encode_wide().collect();
                ensure!(
                    !units.is_empty()
                        && !matches!(units.last(), Some(32 | 46))
                        && !units.iter().any(|unit| {
                            *unit < 32
                                || matches!(*unit, 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124)
                        }),
                    "Windows Installer 路径包含不能安全转换到 DOS/UNC 的文件名"
                );
                normal.push(name);
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                bail!("Windows Installer 路径不能包含相对目录或嵌套前缀");
            }
        }
    }
    let units: Vec<_> = normal.as_os_str().encode_wide().collect();
    ensure!(
        !units.contains(&0) && units.len() < 260,
        "Windows Installer 路径超过 259 个 UTF-16 单元或包含空字符；请使用较短的用户缓存路径"
    );
    Ok(normal)
}

fn validate_installer_boundary(installer: &Path, log: &Path, directory: &Path) -> Result<()> {
    let normal_installer =
        windows_installer_path(installer).context("安装包路径无法交给 Windows Installer")?;
    let normal_log = windows_installer_path(log).context("日志路径无法交给 Windows Installer")?;
    let normal_directory =
        windows_installer_path(directory).context("缓存目录无法交给 Windows Installer")?;
    ensure!(
        normal_installer.canonicalize()? == installer.canonicalize()?,
        "DOS/UNC 安装包路径与已校验文件不一致"
    );
    let canonical_directory = directory.canonicalize()?;
    ensure!(
        normal_directory.canonicalize()? == canonical_directory
            && normal_log
                .parent()
                .context("安装日志目录无效")?
                .canonicalize()?
                == canonical_directory,
        "DOS/UNC 缓存/日志目录与已校验目录不一致"
    );
    Ok(())
}

fn installer_command(
    msiexec: &Path,
    installer: &Path,
    log: &Path,
    directory: &Path,
) -> Result<Command> {
    // Each argument is passed to CreateProcess by std::process::Command;
    // there is no shell interpolation or handwritten Windows quoting.
    let executable = windows_installer_path(msiexec).context("Windows Installer 程序路径无效")?;
    let installer = windows_installer_path(installer)?;
    let log = windows_installer_path(log)?;
    let directory = windows_installer_path(directory)?;
    let mut command = Command::new(executable);
    command
        .arg("/i")
        .arg(installer)
        .arg("/passive")
        .arg("/norestart")
        .arg("/L*v")
        .arg(log)
        .arg("MSIRESTARTMANAGERCONTROL=Disable")
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

fn classify_exit_code(code: u32) -> (InstallStatus, String) {
    match code {
        0 => (InstallStatus::Succeeded, "更新已安装".into()),
        3010 => (
            InstallStatus::RebootRequired,
            "更新已安装，Windows 提示需要稍后重启；本次不会自动重启电脑".into(),
        ),
        1602 => (InstallStatus::Cancelled, "安装已取消".into()),
        code => (
            InstallStatus::Failed,
            format!("Windows Installer 安装失败（错误 {code}）；详情见安装日志"),
        ),
    }
}

fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let directory = path.parent().context("安装结果目录无效")?;
    let temporary = directory.join(format!(".install-result-{}.tmp", unique_id()?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error).context("无法保存安装结果")
        }
    }
}

fn save_result(job: &UpdateJob, result: &InstallResult) -> Result<()> {
    atomic_json(&job.result_path, result)?;
    atomic_json(&job.last_result_path, result)
}

fn installed_registry_directory() -> Result<Option<PathBuf>> {
    let mut buffer = vec![0u16; 32_768];
    let mut bytes = (buffer.len() * 2) as u32;
    let error = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\neko233-com\\SystemMonitorX"),
            w!("InstallPath"),
            RRF_RT_REG_SZ | RRF_NOEXPAND | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    error.ok().context("无法读取安装版应用路径")?;
    ensure!(
        bytes > 0 && bytes.is_multiple_of(2) && bytes as usize <= buffer.len() * 2,
        "安装目录注册值长度无效"
    );
    let units = &buffer[..bytes as usize / 2];
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .context("安装目录注册值缺少结束符")?;
    ensure!(
        end > 0 && units[end..].iter().all(|unit| *unit == 0),
        "安装目录注册值无效"
    );
    use std::os::windows::ffi::OsStringExt;
    Ok(Some(PathBuf::from(OsString::from_wide(&units[..end]))))
}

fn installed_restart_target(
    registered: Option<PathBuf>,
    standard_directory: PathBuf,
) -> Result<PathBuf> {
    // A missing registration uses only the standard per-user installed path.
    // An invalid present value is an error, never a fallback to an old portable
    // executable that would misleadingly appear to have been updated.
    let directory = registered.unwrap_or(standard_directory);
    ensure!(directory.is_absolute(), "安装版目录必须是绝对路径");
    let directory = directory
        .canonicalize()
        .context("安装已完成，但找不到安装版应用目录")?;
    let executable = directory
        .join(APP_NAME)
        .canonicalize()
        .context("安装已完成，但找不到安装版 SystemMonitorX.exe")?;
    ensure!(
        executable.parent() == Some(directory.as_path())
            && executable.is_file()
            && has_filename(&executable, APP_NAME),
        "安装版应用文件名/路径无效"
    );
    Ok(executable)
}

fn restart_application(job: &UpdateJob, installation_succeeded: bool) -> Result<PathBuf> {
    let executable = if installation_succeeded {
        let local = std::env::var_os("LOCALAPPDATA").context("Windows 用户应用数据目录不可用")?;
        installed_restart_target(
            installed_registry_directory()?,
            PathBuf::from(local).join("Programs").join("SystemMonitorX"),
        )?
    } else {
        ensure!(
            job.app_executable.is_file() && has_filename(&job.app_executable, APP_NAME),
            "应用安装路径不存在，无法重新启动"
        );
        job.app_executable.clone()
    };
    Command::new(&executable)
        .current_dir(executable.parent().context("应用目录无效")?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("无法重新启动应用，请从开始菜单打开 SystemMonitorX")?;
    Ok(executable)
}

fn acquire_install_lock(root: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(root.join("install.lock"))
        .context("另一个 SystemMonitorX 窗口正在更新，请等待安装完成后重试")
}

fn open_verified_installer(job: &UpdateJob) -> Result<File> {
    ensure!(
        job.installer_path.canonicalize()? == job.installer_path,
        "安装包路径不能重定向"
    );
    // Deny writes/deletion for the complete verify/install lifetime so the
    // verified bytes cannot be swapped out before msiexec opens the MSI.
    let guard = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&job.installer_path)?;
    verify_file(&job.installer_path, &job.release)?;
    Ok(guard)
}

/// Called only by the copied helper. Does not elevate, kill processes, reboot,
/// or stop anyone else's installer/session. The package is a per-user MSI.
pub fn run_job(path: &Path) -> Result<()> {
    let root = updates_directory()?.canonicalize()?;
    let job_path = path.canonicalize().context("安装任务文件不存在")?;
    let job: UpdateJob = read_bounded_json(&job_path)?;
    validate_job_paths(&job_path, &job, &root)?;
    ensure!(
        job.job_directory.canonicalize()? == job.job_directory,
        "安装助手目录不能重定向到其他目录"
    );
    ensure!(
        std::env::current_exe()?.canonicalize()? == job.helper_executable
            && job.helper_executable.canonicalize()? == job.helper_executable,
        "任务只能由对应缓存中的安装助手执行"
    );
    ensure!(
        job.app_executable.canonicalize()? == job.app_executable,
        "原应用路径不能重定向"
    );
    let mut result = InstallResult {
        job_id: job.job_id.clone(),
        version: job.release.version.clone(),
        status: InstallStatus::Failed,
        exit_code: None,
        message: String::new(),
        log_path: job.log_path.clone(),
        installer_path: job.installer_path.clone(),
        completed_at_unix_secs: 0,
        application_restarted: false,
        restarted_executable: None,
        restart_error: None,
    };
    let startup: Result<(File, File, File)> = (|| {
        let install_lock = acquire_install_lock(&root)?;
        // Retained create-new marker prevents replaying one job twice. A user
        // retry creates a fresh unique job rather than replaying this one.
        let job_lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(job.job_directory.join("install.started"))
            .context("此安装任务已启动，不能重复执行")?;
        let installer_guard = open_verified_installer(&job)?;
        // Repeat the boundary check while the exact verified MSI is guarded,
        // before publishing Ready and allowing the old GUI to exit.
        validate_installer_boundary(&job.installer_path, &job.log_path, &job.job_directory)?;
        let _ = installer_command(
            &system_msiexec()?,
            &job.installer_path,
            &job.log_path,
            &job.job_directory,
        )?;
        atomic_json(
            &job.job_directory.join(READY_NAME),
            &HelperReady {
                job_id: job.job_id.clone(),
                helper_pid: std::process::id(),
                helper_creation_ticks: creation_ticks(unsafe { GetCurrentProcess() })?,
            },
        )?;
        Ok((install_lock, job_lock, installer_guard))
    })();
    let (_install_lock, _job_lock, _installer_guard) = match startup {
        Ok(guards) => guards,
        Err(error) => {
            // No ready acknowledgement means prepare_and_launch keeps the GUI
            // open. Record the concrete failure, with no duplicate restart.
            result.message = format!("安装准备失败，当前窗口保持打开：{error:#}");
            result.completed_at_unix_secs = unix_time();
            let _ = save_result(&job, &result);
            return Err(error);
        }
    };
    let exited = wait_for_exact_process(
        job.old_app_pid,
        job.old_app_creation_ticks,
        Duration::from_secs(30),
    );
    let may_restart = exited.is_ok();
    match exited {
        Err(error) => result.message = format!("{error:#}"),
        Ok(_) => {
            let installed: Result<u32> = (|| {
                let status = installer_command(
                    &system_msiexec()?,
                    &job.installer_path,
                    &job.log_path,
                    &job.job_directory,
                )?
                .status()
                .context("无法启动或等待 Windows Installer")?;
                status
                    .code()
                    .map(|code| code as u32)
                    .context("Windows Installer 没有返回退出码")
            })();
            match installed {
                Ok(code) => {
                    result.exit_code = Some(code);
                    (result.status, result.message) = classify_exit_code(code);
                }
                Err(error) => result.message = format!("更新安装未完成：{error:#}"),
            }
        }
    }
    result.completed_at_unix_secs = unix_time();
    // A result write failure must not strand the user with no app window.
    let first_save = save_result(&job, &result);
    if may_restart {
        match restart_application(&job, result.succeeded()) {
            Ok(executable) => {
                result.application_restarted = true;
                result.restarted_executable = Some(executable);
                result.message.push_str(if result.succeeded() {
                    "；已启动安装版应用"
                } else {
                    "；已重新打开原应用"
                });
            }
            Err(error) => {
                result.restart_error = Some(format!("{error:#}"));
                result
                    .message
                    .push_str("；应用未能重新打开，请从开始菜单启动");
            }
        }
    }
    let final_save = save_result(&job, &result);
    first_save.and(final_save)
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    use windows::Win32::System::Threading::TerminateProcess;

    /// Duplicate the exact Child handle, never find a process by a possibly
    /// recycled PID. The observer also cleans up on assertion failures.
    struct TestChildObserver(HANDLE);
    impl Drop for TestChildObserver {
        fn drop(&mut self) {
            if unsafe { WaitForSingleObject(self.0, 0) } == WAIT_TIMEOUT
                && unsafe { TerminateProcess(self.0, 1) }.is_ok()
            {
                let _ = unsafe { WaitForSingleObject(self.0, 5000) };
            }
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    fn delayed_owned_helper_fixture() -> (PendingHelper, TestChildObserver) {
        let powershell = system_msiexec()
            .unwrap()
            .parent()
            .unwrap()
            .join("WindowsPowerShell\\v1.0\\powershell.exe");
        let child = Command::new(powershell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .unwrap();
        let mut pending = PendingHelper::new(child);
        let mut observed = HANDLE::default();
        // SAFETY: source is exactly the live process handle owned by this Child;
        // the duplicate supplies test-only observation and cleanup ownership.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(pending.child_mut().as_raw_handle()),
                GetCurrentProcess(),
                &mut observed,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .unwrap();
        (pending, TestChildObserver(observed))
    }

    #[test]
    fn failed_helper_handoff_reaps_its_exact_delayed_child() {
        let (pending, observed) = delayed_owned_helper_fixture();
        assert_eq!(unsafe { WaitForSingleObject(observed.0, 0) }, WAIT_TIMEOUT);
        drop(pending); // Same cleanup used by every preparation error/timeout.
        assert_eq!(
            unsafe { WaitForSingleObject(observed.0, 5000) },
            WAIT_OBJECT_0
        );
    }

    #[test]
    fn successful_helper_handoff_detaches_without_terminating_child() {
        let (pending, observed) = delayed_owned_helper_fixture();
        let detached = pending.detach();
        // Retain test-only cleanup ownership before making assertions.
        let mut cleanup = PendingHelper::new(detached);
        assert!(cleanup.child_mut().try_wait().unwrap().is_none());
        assert_eq!(unsafe { WaitForSingleObject(observed.0, 0) }, WAIT_TIMEOUT);
        drop(cleanup);
        assert_eq!(
            unsafe { WaitForSingleObject(observed.0, 5000) },
            WAIT_OBJECT_0
        );
    }

    fn release(bytes: &[u8]) -> ReleaseInfo {
        ReleaseInfo { version: "1.2.3".into(), page_url: "https://github.com/neko233-com/SystemMonitorX/releases/tag/v1.2.3".into(),
            installer_name: "SystemMonitorX-1.2.3-x64.msi".into(), installer_url: "https://github.com/neko233-com/SystemMonitorX/releases/download/v1.2.3/SystemMonitorX-1.2.3-x64.msi".into(),
            installer_sha256: format!("{:x}", Sha256::digest(bytes)), installer_bytes: bytes.len() as u64 }
    }

    fn fixture_job(root: &Path) -> UpdateJob {
        let id = "1".repeat(32);
        let directory = root.join(format!("helper-{id}"));
        UpdateJob {
            format_version: JOB_VERSION,
            job_id: id,
            job_directory: directory.clone(),
            installer_path: root.join("1.2.3").join("SystemMonitorX-1.2.3-x64.msi"),
            release: release(b"fixture"),
            old_app_pid: 1234,
            old_app_creation_ticks: 133_000_000_000_000_000,
            app_executable: PathBuf::from(
                "C:\\Users\\Sample User\\AppData\\Local\\Programs\\SystemMonitorX\\SystemMonitorX.exe",
            ),
            helper_executable: directory.join(HELPER_NAME),
            result_path: directory.join(RESULT_NAME),
            last_result_path: root.join(LAST_RESULT_NAME),
            log_path: directory.join(LOG_NAME),
        }
    }

    #[test]
    fn jobs_reject_path_and_release_injection() {
        let root = PathBuf::from("C:\\Users\\Sample User\\AppData\\Local\\SystemMonitorX\\updates");
        let job = fixture_job(&root);
        let path = job.job_directory.join(JOB_NAME);
        validate_job_paths(&path, &job, &root).unwrap();
        let mut escaped = job.clone();
        escaped.log_path = root.join("..\\other.log");
        assert!(validate_job_paths(&path, &escaped, &root).is_err());
        let mut foreign = job.clone();
        foreign.app_executable = PathBuf::from("C:\\Windows\\System32\\cmd.exe");
        assert!(validate_job_paths(&path, &foreign, &root).is_err());
        let mut foreign = job.clone();
        foreign.release.installer_url = "https://example.invalid/setup.msi".into();
        assert!(validate_job_paths(&path, &foreign, &root).is_err());
        let mut forged = job.clone();
        forged.job_id = "..\\escape".into();
        assert!(validate_job_paths(&path, &forged, &root).is_err());
        assert!(!runtime_filename("evil.dll"));
        assert!(!runtime_filename("vcruntime140.dll/evil.exe"));
        assert!(runtime_filename("vcruntime140_1.dll"));
    }

    #[test]
    fn installer_arguments_preserve_spaces_and_shell_metacharacters() {
        let executable = Path::new("C:\\Windows\\System32\\msiexec.exe");
        let installer = Path::new("C:\\User Files\\A & B\\SystemMonitorX-1.2.3-x64.msi");
        let log = Path::new("C:\\User Files\\helper cache\\install.log");
        let command = installer_command(
            executable,
            installer,
            log,
            Path::new("C:\\User Files\\helper cache"),
        )
        .unwrap();
        assert_eq!(command.get_program(), executable.as_os_str());
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_os_string())
            .collect();
        assert_eq!(args[1], installer.as_os_str());
        assert_eq!(args[5], log.as_os_str());
        assert_eq!(args.len(), 7);
        assert_eq!(args[6], "MSIRESTARTMANAGERCONTROL=Disable");
    }

    #[test]
    fn installer_boundary_converts_verbatim_dos_paths_without_losing_argument_identity() {
        let command = installer_command(
            Path::new(r"\\?\C:\Windows\System32\msiexec.exe"),
            Path::new(r"\\?\C:\User Files\甲 & 乙\SystemMonitorX-1.2.3-x64.msi"),
            Path::new(r"\\?\C:\User Files\helper cache\install.log"),
            Path::new(r"\\?\C:\User Files\helper cache"),
        )
        .unwrap();
        assert_eq!(
            command.get_program(),
            Path::new(r"C:\Windows\System32\msiexec.exe")
        );
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(
            arguments[1],
            Path::new(r"C:\User Files\甲 & 乙\SystemMonitorX-1.2.3-x64.msi")
        );
        assert_eq!(
            arguments[5],
            Path::new(r"C:\User Files\helper cache\install.log")
        );
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new(r"C:\User Files\helper cache"))
        );
        assert_eq!(
            windows_installer_path(Path::new(r"C:\User Files\甲 & 乙\app.msi")).unwrap(),
            Path::new(r"C:\User Files\甲 & 乙\app.msi")
        );
    }

    #[test]
    fn installer_boundary_preserves_unc_server_share_and_unicode_spaces() {
        let expected = Path::new(r"\\file server\Shared Files\更新目录\app.msi");
        assert_eq!(
            windows_installer_path(Path::new(
                r"\\?\UNC\file server\Shared Files\更新目录\app.msi"
            ))
            .unwrap(),
            expected
        );
        assert_eq!(windows_installer_path(expected).unwrap(), expected);
        let command = installer_command(
            Path::new(r"C:\Windows\System32\msiexec.exe"),
            Path::new(r"\\?\UNC\server\share\update cache\app.msi"),
            Path::new(r"\\?\UNC\server\share\update cache\install.log"),
            Path::new(r"\\?\UNC\server\share\update cache"),
        )
        .unwrap();
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new(r"\\server\share\update cache"))
        );
        assert_eq!(
            command.get_args().nth(1).unwrap(),
            Path::new(r"\\server\share\update cache\app.msi")
        );
    }

    #[test]
    fn installer_boundary_rejects_device_relative_ambiguous_and_overlong_paths_before_spawn() {
        for path in [
            r"\\.\PhysicalDrive0",
            r"\\.\pipe\install",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\app.msi",
            r"\\?\Volume{11111111-1111-1111-1111-111111111111}\app.msi",
            r"C:relative.msi",
            r"\rooted-without-drive.msi",
            "relative.msi",
            r"\\?\C:\path.\app.msi",
            r"\\?\C:\path \app.msi",
            r"\\?\C:\folder\..\app.msi",
        ] {
            assert!(windows_installer_path(Path::new(path)).is_err(), "{path}");
        }
        let long = format!(r"\\?\C:\{}\app.msi", "a".repeat(260));
        assert!(windows_installer_path(Path::new(&long)).is_err());
        assert!(
            installer_command(
                Path::new(r"C:\Windows\System32\msiexec.exe"),
                Path::new(r"C:\update\app.msi"),
                Path::new(r"\\.\pipe\log"),
                Path::new(r"C:\update"),
            )
            .is_err()
        );
    }

    #[test]
    fn installer_exit_codes_distinguish_success_reboot_cancel_and_failure() {
        assert_eq!(classify_exit_code(0).0, InstallStatus::Succeeded);
        assert_eq!(classify_exit_code(3010).0, InstallStatus::RebootRequired);
        assert_eq!(classify_exit_code(1602).0, InstallStatus::Cancelled);
        assert_eq!(classify_exit_code(1603).0, InstallStatus::Failed);
    }

    #[test]
    fn helper_rejects_bad_hash_without_starting_installer() {
        let directory =
            std::env::temp_dir().join(format!("SystemMonitorX-hash-test-{}", unique_id().unwrap()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("SystemMonitorX-1.2.3-x64.msi");
        fs::write(&path, b"tampered").unwrap();
        assert!(verify_file(&path, &release(b"original")).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn install_lock_is_exclusive_and_reusable_after_drop() {
        let directory =
            std::env::temp_dir().join(format!("SystemMonitorX-lock-test-{}", unique_id().unwrap()));
        fs::create_dir(&directory).unwrap();
        let first = acquire_install_lock(&directory).unwrap();
        assert!(acquire_install_lock(&directory).is_err());
        drop(first);
        let second = acquire_install_lock(&directory).unwrap();
        drop(second);
        fs::remove_file(directory.join("install.lock")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn verified_msi_guard_blocks_changes_and_rejects_tampering() {
        let directory = std::env::temp_dir().join(format!(
            "SystemMonitorX-msi-lock-test-{}",
            unique_id().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("SystemMonitorX-1.2.3-x64.msi");
        fs::write(&path, b"original").unwrap();
        let mut job = fixture_job(&directory);
        job.installer_path = path.canonicalize().unwrap();
        job.release = release(b"original");
        let guard = open_verified_installer(&job).unwrap();
        assert!(
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .is_err()
        );
        assert!(fs::rename(&path, directory.join("replacement.msi")).is_err());
        drop(guard);
        fs::write(&path, b"tampered").unwrap();
        assert!(open_verified_installer(&job).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cache_copies_only_bundled_helper_and_vc_runtime_files() {
        let directory =
            std::env::temp_dir().join(format!("SystemMonitorX-copy-test-{}", unique_id().unwrap()));
        let application = directory.join("installed");
        let cache = directory.join("cache");
        fs::create_dir_all(&application).unwrap();
        fs::create_dir(&cache).unwrap();
        for name in [
            HELPER_NAME,
            "vcruntime140_1.dll",
            "msvcp140.dll",
            "other.dll",
            APP_NAME,
            "private.txt",
        ] {
            fs::write(application.join(name), b"copy-test-only-not-executed").unwrap();
        }
        let helper = copy_bundled_helper(&application.canonicalize().unwrap(), &cache).unwrap();
        assert_eq!(helper, cache.join(HELPER_NAME).canonicalize().unwrap());
        assert!(cache.join("vcruntime140_1.dll").is_file());
        assert!(cache.join("msvcp140.dll").is_file());
        assert!(!cache.join("other.dll").exists());
        assert!(!cache.join(APP_NAME).exists());
        assert!(!cache.join("private.txt").exists());
        for entry in fs::read_dir(&application).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        for entry in fs::read_dir(&cache).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(application).unwrap();
        fs::remove_dir(cache).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn successful_portable_update_selects_installed_target_and_rejects_missing_target() {
        let root = std::env::temp_dir().join(format!(
            "SystemMonitorX-target-test-{}",
            unique_id().unwrap()
        ));
        let standard = root.join("standard");
        let registered = root.join("registered");
        fs::create_dir_all(&standard).unwrap();
        fs::create_dir(&registered).unwrap();
        fs::write(standard.join(APP_NAME), b"path-test-only-not-executed").unwrap();
        fs::write(registered.join(APP_NAME), b"path-test-only-not-executed").unwrap();
        assert_eq!(
            installed_restart_target(None, standard.clone()).unwrap(),
            standard.join(APP_NAME).canonicalize().unwrap()
        );
        assert_eq!(
            installed_restart_target(Some(registered.clone()), standard.clone()).unwrap(),
            registered.join(APP_NAME).canonicalize().unwrap()
        );
        assert!(
            installed_restart_target(Some(PathBuf::from("relative")), standard.clone()).is_err()
        );
        fs::remove_file(registered.join(APP_NAME)).unwrap();
        assert!(installed_restart_target(Some(registered.clone()), standard.clone()).is_err());
        fs::remove_file(standard.join(APP_NAME)).unwrap();
        fs::remove_dir(standard).unwrap();
        fs::remove_dir(registered).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn waits_only_for_owned_child_exact_creation_identity() {
        let cmd = system_msiexec().unwrap().parent().unwrap().join("cmd.exe");
        // Keep the child blocked on our owned stdin pipe until both identity
        // checks complete; no interpreter cold-start duration is assumed.
        let child = Command::new(cmd)
            .args(["/D", "/Q", "/C", "set /p SystemMonitorXTestWait="])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .unwrap();
        // An assertion failure still kills/reaps only this exact Child handle.
        let mut pending = PendingHelper::new(child);
        let child = pending.child_mut();
        let handle = OwnedHandle(
            unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    false,
                    child.id(),
                )
            }
            .unwrap(),
        );
        let ticks = creation_ticks(handle.0).unwrap();
        assert_eq!(
            wait_for_exact_process(child.id(), ticks + 1, Duration::from_millis(0)).unwrap(),
            OriginalProcessExit::PidReused
        );
        assert!(wait_for_exact_process(child.id(), ticks, Duration::from_millis(0)).is_err());
        // EOF releases the fixture deterministically, even on slow CI hosts.
        drop(child.stdin.take());
        assert_eq!(
            wait_for_exact_process(child.id(), ticks, Duration::from_secs(10)).unwrap(),
            OriginalProcessExit::Exited
        );
        child.wait().unwrap();
    }
}

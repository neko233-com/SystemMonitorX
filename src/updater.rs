//! Background release checks and verified, user-requested MSI downloads.
//! WinHTTP runs only on the updater thread, with timeouts and bounded bodies.
use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{Win32::Networking::WinHttp::*, core::PCWSTR};

const REPOSITORY: &str = "neko233-com/SystemMonitorX";
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);
const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub page_url: String,
    pub installer_name: String,
    pub installer_url: String,
    pub installer_sha256: String,
    pub installer_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub enum UpdatePhase {
    #[default]
    Idle,
    Checking,
    Current,
    Available,
    Downloading {
        received: u64,
        total: u64,
    },
    Ready(PathBuf),
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct UpdateSnapshot {
    pub sequence: u64,
    pub enabled: bool,
    pub checked_at: Option<u64>,
    pub release: Option<ReleaseInfo>,
    pub phase: UpdatePhase,
    pub ready_package: Option<PathBuf>,
}

enum Request {
    Check,
    SettingsChanged,
    Download,
    Stop,
}

pub struct UpdateService {
    state: Arc<Mutex<UpdateSnapshot>>,
    sender: mpsc::SyncSender<Request>,
}
impl Drop for UpdateService {
    fn drop(&mut self) {
        let _ = self.sender.try_send(Request::Stop);
    }
}
impl UpdateService {
    pub fn new(enabled: bool) -> Self {
        let state = Arc::new(Mutex::new(UpdateSnapshot {
            enabled,
            ..Default::default()
        }));
        let (sender, receiver) = mpsc::sync_channel(4);
        let worker_state = state.clone();
        // Network and installer hashing never execute in the GPUI event loop.
        let worker = std::thread::Builder::new()
            .name("release-updater".into())
            .spawn(move || {
                let mut cache: CheckCache = fs::read(cache_path())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();

                let now = unix_time();
                let remaining = CHECK_INTERVAL
                    .as_secs()
                    .saturating_sub(now.saturating_sub(cache.checked_at));
                let mut due =
                    Instant::now() + Duration::from_secs(if remaining > 0 { remaining } else { 5 });
                if let Some(release) = cache.release.clone() {
                    publish(&worker_state, |state| {
                        state.checked_at = Some(cache.checked_at);
                        state.phase = release_phase(&release);
                        state.release = Some(release);
                    });
                }
                loop {
                    let automatic = worker_state
                        .lock()
                        .map(|state| state.enabled)
                        .unwrap_or(false);
                    let timeout = if automatic {
                        due.saturating_duration_since(Instant::now())
                    } else {
                        CHECK_INTERVAL
                    };
                    let request = match receiver.recv_timeout(timeout) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) if automatic => Request::Check,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match request {
                        Request::Stop => break,
                        Request::SettingsChanged => {
                            if worker_state
                                .lock()
                                .map(|state| state.enabled)
                                .unwrap_or(false)
                            {
                                due = Instant::now();
                            }
                        }
                        Request::Check => {
                            publish(&worker_state, |state| state.phase = UpdatePhase::Checking);
                            match check_latest(cache.etag.as_deref()) {
                                Ok(CheckResponse::New(release, etag)) => {
                                    cache = CheckCache {
                                        checked_at: unix_time(),
                                        etag,
                                        release: Some(release.clone()),
                                    };
                                    publish(&worker_state, |state| {
                                        state.checked_at = Some(cache.checked_at);
                                        accept_release(state, release, cache.checked_at);
                                    });
                                    let _ = save_cache(&cache);
                                    due = Instant::now() + CHECK_INTERVAL;
                                }
                                Ok(CheckResponse::Unchanged) if cache.release.is_some() => {
                                    cache.checked_at = unix_time();
                                    publish(&worker_state, |state| {
                                        state.checked_at = Some(cache.checked_at);
                                        accept_release(
                                            state,
                                            cache.release.as_ref().unwrap().clone(),
                                            cache.checked_at,
                                        );
                                    });
                                    let _ = save_cache(&cache);
                                    due = Instant::now() + CHECK_INTERVAL;
                                }
                                Ok(CheckResponse::Unchanged) => {
                                    cache.etag = None;
                                    due = Instant::now() + RETRY_INTERVAL;
                                    publish(&worker_state, |state| {
                                        state.phase =
                                            UpdatePhase::Failed("更新缓存缺失，将重新检查".into())
                                    });
                                }
                                Err(error) => {
                                    due = Instant::now() + RETRY_INTERVAL;
                                    publish(&worker_state, |state| {
                                        state.phase = UpdatePhase::Failed(format!("{error:#}"))
                                    });
                                }
                            }
                        }
                        Request::Download => {
                            let Some(release) = cache.release.clone() else {
                                continue;
                            };
                            publish(&worker_state, |state| {
                                state.phase = UpdatePhase::Downloading {
                                    received: 0,
                                    total: release.installer_bytes,
                                }
                            });
                            match download_verified(
                                &release,
                                &update_directory(),
                                |received, total| {
                                    publish(&worker_state, |state| {
                                        state.phase = UpdatePhase::Downloading { received, total }
                                    });
                                },
                            ) {
                                Ok(path) => publish(&worker_state, |state| {
                                    state.ready_package = Some(path.clone());
                                    state.phase = UpdatePhase::Ready(path);
                                }),
                                Err(error) => publish(&worker_state, |state| {
                                    state.phase = UpdatePhase::Failed(format!("{error:#}"))
                                }),
                            }
                        }
                    }
                }
            });
        if let Err(error) = worker {
            publish(&state, |state| {
                state.phase = UpdatePhase::Failed(format!("更新线程未启动：{error}"))
            });
        }
        Self { state, sender }
    }
    pub fn snapshot(&self) -> UpdateSnapshot {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
    pub fn check(&self) {
        let _ = self.sender.try_send(Request::Check);
    }
    pub fn set_enabled(&self, enabled: bool) {
        publish(&self.state, |state| state.enabled = enabled);
        let _ = self.sender.try_send(Request::SettingsChanged);
    }
    pub fn download(&self) {
        let _ = self.sender.try_send(Request::Download);
    }
}
fn publish(state: &Arc<Mutex<UpdateSnapshot>>, update: impl FnOnce(&mut UpdateSnapshot)) {
    if let Ok(mut state) = state.lock() {
        update(&mut state);
        state.sequence = state.sequence.wrapping_add(1);
    }
}
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn update_directory() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into()))
        .join("SystemMonitorX/updates")
}
fn cache_path() -> PathBuf {
    update_directory().join("release-cache.json")
}
#[derive(Default, Serialize, Deserialize)]
struct CheckCache {
    checked_at: u64,
    etag: Option<String>,
    release: Option<ReleaseInfo>,
}
fn save_cache(cache: &CheckCache) -> Result<()> {
    fs::create_dir_all(update_directory())?;
    fs::write(cache_path(), serde_json::to_vec(cache)?)?;
    Ok(())
}
fn accept_release(state: &mut UpdateSnapshot, release: ReleaseInfo, checked_at: u64) {
    let previous_ready = if state.release.as_ref() == Some(&release) {
        state.ready_package.clone().or_else(|| match &state.phase {
            UpdatePhase::Ready(path) => Some(path.clone()),
            _ => None,
        })
    } else {
        None
    };
    state.ready_package = previous_ready.clone();
    state.checked_at = Some(checked_at);
    state.phase = previous_ready
        .map(UpdatePhase::Ready)
        .unwrap_or_else(|| release_phase(&release));
    state.release = Some(release);
}
fn release_phase(release: &ReleaseInfo) -> UpdatePhase {
    if is_newer(&release.version, env!("CARGO_PKG_VERSION")).unwrap_or(false) {
        UpdatePhase::Available
    } else {
        UpdatePhase::Current
    }
}
pub fn is_newer(remote: &str, current: &str) -> Result<bool> {
    let remote = Version::parse(remote.trim_start_matches('v'))?;
    let current = Version::parse(current.trim_start_matches('v'))?;
    Ok(remote.pre.is_empty() && remote.cmp_precedence(&current) == std::cmp::Ordering::Greater)
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ApiAsset>,
}
#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    state: String,
    digest: Option<String>,
}
pub fn parse_release(bytes: &[u8]) -> Result<ReleaseInfo> {
    let release: ApiRelease =
        serde_json::from_slice(bytes).context("更新服务器返回了无效版本信息")?;
    ensure!(release.tag_name.starts_with('v'), "版本标签必须以 v 开头");
    let version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )?;
    ensure!(
        !release.draft && !release.prerelease && version.pre.is_empty() && version.build.is_empty(),
        "该版本不是稳定发布"
    );
    let version = version.to_string();
    let page_url = format!(
        "https://github.com/{REPOSITORY}/releases/tag/{}",
        release.tag_name
    );
    ensure!(release.html_url == page_url, "更新地址不属于本项目");
    let installer_name = format!("SystemMonitorX-{version}-x64.msi");
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == installer_name && a.state == "uploaded")
        .context("新版本安装包尚未发布")?;
    let installer_url = format!(
        "https://github.com/{REPOSITORY}/releases/download/{}/{}",
        release.tag_name, installer_name
    );
    ensure!(
        asset.browser_download_url == installer_url,
        "安装包地址不属于本项目发布"
    );
    ensure!(
        asset.size > 0 && asset.size <= MAX_INSTALLER_BYTES,
        "安装包大小无效"
    );
    let digest = asset
        .digest
        .and_then(|d| d.strip_prefix("sha256:").map(str::to_owned))
        .context("安装包缺少 SHA-256 校验值")?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "安装包校验值格式无效"
    );
    Ok(ReleaseInfo {
        version,
        page_url,
        installer_name,
        installer_url,
        installer_sha256: digest.to_ascii_lowercase(),
        installer_bytes: asset.size,
    })
}
pub enum CheckResponse {
    New(ReleaseInfo, Option<String>),
    Unchanged,
}
pub fn check_latest(etag: Option<&str>) -> Result<CheckResponse> {
    let path = format!("/repos/{REPOSITORY}/releases/latest");
    let request = HttpRequest::get("api.github.com", &path, etag)?;
    let status = request.status()?;
    if status == 304 {
        return Ok(CheckResponse::Unchanged);
    }
    // Public GitHub API quotas are shared by an egress IP. A token must never
    // be required or copied from the developer's gh login into a shipped app.
    if matches!(status, 403 | 429) {
        return check_release_downloads().map(|release| CheckResponse::New(release, None));
    }
    ensure!(
        status == 200,
        "GitHub 更新检查失败（HTTP {status}），稍后自动重试"
    );
    let etag = request.etag();
    let mut bytes = Vec::new();
    request.read(512 * 1024, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(CheckResponse::New(parse_release(&bytes)?, etag))
}
fn check_release_downloads() -> Result<ReleaseInfo> {
    let request = HttpRequest::head("github.com", &format!("/{REPOSITORY}/releases/latest"))?;
    ensure!(request.status()? == 200, "稳定版发布入口暂不可用");
    let page_url = request.effective_url()?;
    let prefix = format!("https://github.com/{REPOSITORY}/releases/tag/v");
    let version = Version::parse(
        page_url
            .strip_prefix(&prefix)
            .context("稳定版入口跳转到非项目地址")?,
    )?;
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "备用入口不是稳定版"
    );
    let version = version.to_string();
    let base = format!("/{REPOSITORY}/releases/download/v{version}");
    let checksums = HttpRequest::get("github.com", &format!("{base}/SHA256SUMS.txt"), None)?;
    ensure!(checksums.status()? == 200, "稳定版尚未提供校验文件");
    let mut bytes = Vec::new();
    checksums.read(16 * 1024, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    let installer_name = format!("SystemMonitorX-{version}-x64.msi");
    let installer_sha256 = checksum_for(std::str::from_utf8(&bytes)?, &installer_name)?;
    let installer_path = format!("{base}/{installer_name}");
    let installer = HttpRequest::head("github.com", &installer_path)?;
    ensure!(installer.status()? == 200, "稳定版安装包暂不可用");
    let installer_bytes = installer.content_length().context("安装包未提供大小")?;
    ensure!(
        installer_bytes > 0 && installer_bytes <= MAX_INSTALLER_BYTES,
        "安装包大小无效"
    );
    Ok(ReleaseInfo {
        version,
        page_url,
        installer_name,
        installer_url: format!("https://github.com{installer_path}"),
        installer_sha256,
        installer_bytes,
    })
}
fn checksum_for(text: &str, filename: &str) -> Result<String> {
    let mut matches = text.lines().filter_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == filename && parts.next().is_none()).then_some(hash)
    });
    let hash = matches.next().context("校验文件缺少当前架构的安装包")?;
    ensure!(matches.next().is_none(), "校验文件包含重复的安装包记录");
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "校验文件 SHA-256 无效"
    );
    Ok(hash.to_ascii_lowercase())
}
pub fn download_verified(
    release: &ReleaseInfo,
    directory: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf> {
    let version = Version::parse(&release.version)?;
    let expected_name = format!("SystemMonitorX-{version}-x64.msi");
    ensure!(release.installer_name == expected_name, "更新文件名无效");
    let prefix = format!("https://github.com/{REPOSITORY}/releases/download/v{version}/");
    ensure!(
        release.installer_url == format!("{prefix}{expected_name}"),
        "更新下载地址无效"
    );
    ensure!(
        release.installer_bytes > 0 && release.installer_bytes <= MAX_INSTALLER_BYTES,
        "更新文件大小无效"
    );
    let folder = directory.join(version.to_string());
    fs::create_dir_all(&folder)?;
    let path = folder.join(&expected_name);
    if path.is_file() && verify_file(&path, release).is_ok() {
        return Ok(path);
    }
    let part = unique_download_path(&folder, &expected_name);
    let _cleanup = TemporaryDownload(part.clone());
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .context("无法写入更新缓存目录")?;
    let request_path = release
        .installer_url
        .strip_prefix("https://github.com")
        .context("更新下载地址无效")?;
    let request = HttpRequest::get("github.com", request_path, None)?;
    ensure!(request.status()? == 200, "安装包下载失败");
    let mut received = 0;
    let mut hash = Sha256::new();
    let mut last_progress = Instant::now();
    request.read(release.installer_bytes, |chunk| {
        file.write_all(chunk)?;
        hash.update(chunk);
        received += chunk.len() as u64;
        if last_progress.elapsed() >= Duration::from_millis(200) {
            progress(received, release.installer_bytes);
            last_progress = Instant::now();
        }
        Ok(())
    })?;
    file.sync_all()?;
    drop(file);
    ensure!(received == release.installer_bytes, "安装包下载不完整");
    ensure!(
        format!("{:x}", hash.finalize()) == release.installer_sha256,
        "SHA-256 不匹配，拒绝安装"
    );
    // Only this app's versioned download cache is replaced, after verification.
    if verify_file(&path, release).is_ok() {
        return Ok(path);
    }
    fs::rename(&part, &path)?;
    progress(received, received);
    Ok(path)
}
struct TemporaryDownload(PathBuf);
impl Drop for TemporaryDownload {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn unique_download_path(folder: &Path, filename: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    folder.join(format!(
        ".{filename}.{}.{epoch}.{sequence}.download",
        std::process::id()
    ))
}
pub fn verify_file(path: &Path, release: &ReleaseInfo) -> Result<()> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() == release.installer_bytes,
        "安装包大小不符"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    ensure!(
        format!("{:x}", hash.finalize()) == release.installer_sha256,
        "安装包 SHA-256 不匹配"
    );
    Ok(())
}

/// Called only after the user presses Install; recheck bytes before launch.
/// This blocking verification belongs on a background task too.
pub fn launch_installer(path: &Path, release: &ReleaseInfo) -> Result<()> {
    verify_file(path, release)?;
    let directory = update_directory().canonicalize()?;
    let path = path.canonicalize()?;
    ensure!(
        path.starts_with(&directory)
            && path
                .file_name()
                .is_some_and(|n| n == release.installer_name.as_str()),
        "安装包不在应用更新目录内"
    );
    crate::update_install::prepare_and_launch(&path, release)?;
    Ok(())
}

struct HttpHandle(*mut c_void);
impl Drop for HttpHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}
struct HttpRequest {
    request: HttpHandle,
    _connection: HttpHandle,
    _session: HttpHandle,
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn handle(value: *mut c_void) -> Result<HttpHandle> {
    if value.is_null() {
        bail!("网络请求初始化失败：{}", std::io::Error::last_os_error())
    }
    Ok(HttpHandle(value))
}
impl HttpRequest {
    fn get(host: &str, path: &str, etag: Option<&str>) -> Result<Self> {
        Self::send(host, path, etag, "GET")
    }
    fn head(host: &str, path: &str) -> Result<Self> {
        Self::send(host, path, None, "HEAD")
    }
    fn send(host: &str, path: &str, etag: Option<&str>, verb: &str) -> Result<Self> {
        let agent = wide(concat!("SystemMonitorX/", env!("CARGO_PKG_VERSION")));
        let host = wide(host);
        let path = wide(path);
        let verb = wide(verb);
        let session = handle(unsafe {
            WinHttpOpen(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        })?;
        unsafe { WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 10000) }
            .context("设置更新请求超时失败")?;
        let connection =
            handle(unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0) })?;
        let request = handle(unsafe {
            WinHttpOpenRequest(
                connection.0,
                PCWSTR(verb.as_ptr()),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        })?;
        unsafe {
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_ne_bytes()),
            )
        }?;
        let mut headers =
            "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2026-03-10\r\n"
                .to_owned();
        if let Some(etag) = etag.filter(|s| !s.contains(['\r', '\n', '\0'])) {
            headers.push_str(&format!("If-None-Match: {etag}\r\n"));
        }
        let headers: Vec<u16> = headers.encode_utf16().collect();
        unsafe { WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0) }
            .context("无法连接更新服务器")?;
        unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
            .context("读取更新响应失败")?;
        Ok(Self {
            request,
            _connection: connection,
            _session: session,
        })
    }
    fn status(&self) -> Result<u32> {
        let mut status = 0u32;
        let mut len = 4u32;
        unsafe {
            WinHttpQueryHeaders(
                self.request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut status as *mut u32).cast()),
                &mut len,
                std::ptr::null_mut(),
            )
        }?;
        Ok(status)
    }
    fn etag(&self) -> Option<String> {
        let mut buffer = [0u16; 256];
        let mut len = (buffer.len() * 2) as u32;
        unsafe {
            WinHttpQueryHeaders(
                self.request.0,
                WINHTTP_QUERY_ETAG,
                PCWSTR::null(),
                Some(buffer.as_mut_ptr().cast()),
                &mut len,
                std::ptr::null_mut(),
            )
        }
        .ok()?;
        let value = String::from_utf16_lossy(
            &buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
        );
        (!value.contains(['\r', '\n', '\0'])).then_some(value)
    }
    fn effective_url(&self) -> Result<String> {
        let mut buffer = [0u16; 1024];
        let mut len = (buffer.len() * 2) as u32;
        unsafe {
            WinHttpQueryOption(
                self.request.0,
                WINHTTP_OPTION_URL,
                Some(buffer.as_mut_ptr().cast()),
                &mut len,
            )
        }?;
        Ok(String::from_utf16_lossy(
            &buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
        ))
    }
    fn content_length(&self) -> Option<u64> {
        let mut buffer = [0u16; 64];
        let mut len = (buffer.len() * 2) as u32;
        unsafe {
            WinHttpQueryHeaders(
                self.request.0,
                WINHTTP_QUERY_CONTENT_LENGTH,
                PCWSTR::null(),
                Some(buffer.as_mut_ptr().cast()),
                &mut len,
                std::ptr::null_mut(),
            )
        }
        .ok()?;
        String::from_utf16_lossy(&buffer[..buffer.iter().position(|c| *c == 0)?])
            .parse()
            .ok()
    }
    fn read(&self, limit: u64, mut on_chunk: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let started = Instant::now();
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(120),
                "下载超时，请重试"
            );
            let mut n = 0u32;
            unsafe {
                WinHttpReadData(
                    self.request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut n,
                )
            }
            .context("下载连接中断")?;
            if n == 0 {
                return Ok(());
            }
            total += n as u64;
            ensure!(
                n as usize <= buffer.len() && total <= limit,
                "更新响应超过预期大小"
            );
            on_chunk(&buffer[..n as usize])?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release() -> serde_json::Value {
        serde_json::json!({"tag_name":"v0.2.0","html_url":"https://github.com/neko233-com/SystemMonitorX/releases/tag/v0.2.0","draft":false,"prerelease":false,"assets":[{"name":"SystemMonitorX-0.2.0-x64.msi","browser_download_url":"https://github.com/neko233-com/SystemMonitorX/releases/download/v0.2.0/SystemMonitorX-0.2.0-x64.msi","size":7,"state":"uploaded","digest":format!("sha256:{}", "a".repeat(64))}]})
    }
    #[test]
    fn compares_semantic_versions_and_excludes_prereleases() {
        assert!(is_newer("v0.10.0", "0.2.0").unwrap());
        assert!(!is_newer("0.2.0-beta.1", "0.1.0").unwrap());
        assert!(!is_newer("0.1.0", "0.1.0").unwrap());
        assert!(!is_newer("0.1.0", "0.2.0").unwrap());
    }
    #[test]
    fn rejects_external_urls_missing_hash_and_unpublished_assets() {
        let value = release();
        assert_eq!(
            parse_release(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .version,
            "0.2.0"
        );
        for (field, bad) in [
            (
                "browser_download_url",
                serde_json::json!("https://evil.test/update.msi"),
            ),
            ("digest", serde_json::Value::Null),
            ("state", serde_json::json!("new")),
        ] {
            let mut value = release();
            value["assets"][0][field] = bad;
            assert!(parse_release(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let mut value = release();
        value["draft"] = serde_json::json!(true);
        assert!(parse_release(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn damaged_file_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "SystemMonitorX-hash-test-{}.bin",
            std::process::id()
        ));
        fs::write(&path, b"content").unwrap();
        let mut info = parse_release(&serde_json::to_vec(&release()).unwrap()).unwrap();
        info.installer_sha256 = format!("{:x}", Sha256::digest(b"content"));
        assert!(verify_file(&path, &info).is_ok());
        fs::write(&path, b"changed").unwrap();
        assert!(verify_file(&path, &info).is_err());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn checksum_fallback_requires_one_exact_file() {
        let hash = "b".repeat(64);
        assert_eq!(
            checksum_for(&format!("{hash}  app.msi\n"), "app.msi").unwrap(),
            hash
        );
        assert!(checksum_for(&format!("{hash}  other.msi\n"), "app.msi").is_err());
        assert!(checksum_for(&format!("{hash} app.msi\n{hash} app.msi"), "app.msi").is_err());
        assert!(checksum_for("invalid app.msi", "app.msi").is_err());
    }
    #[test]
    fn metadata_does_not_change_semver_precedence() {
        assert!(!is_newer("0.2.0+build.2", "0.2.0+build.1").unwrap());
        assert!(is_newer("0.2.1", "0.2.0+build.1").unwrap());
        let mut data = release();
        data["tag_name"] = serde_json::json!("0.2.0");
        data["html_url"] =
            serde_json::json!("https://github.com/neko233-com/SystemMonitorX/releases/tag/0.2.0");
        data["assets"][0]["browser_download_url"] = serde_json::json!(
            "https://github.com/neko233-com/SystemMonitorX/releases/download/0.2.0/SystemMonitorX-0.2.0-x64.msi"
        );
        assert!(parse_release(&serde_json::to_vec(&data).unwrap()).is_err());
    }
    #[test]
    fn checks_preserve_a_ready_package_only_for_the_identical_release() {
        let info = parse_release(&serde_json::to_vec(&release()).unwrap()).unwrap();
        let path = PathBuf::from("verified.msi");
        let mut state = UpdateSnapshot {
            release: Some(info.clone()),
            ready_package: Some(path.clone()),
            phase: UpdatePhase::Checking,
            ..Default::default()
        };
        accept_release(&mut state, info.clone(), 10);
        assert!(matches!(&state.phase,UpdatePhase::Ready(p) if p==&path));
        let mut changed = info;
        changed.installer_sha256 = "b".repeat(64);
        accept_release(&mut state, changed, 20);
        assert!(!matches!(state.phase, UpdatePhase::Ready(_)));
    }
    #[test]
    fn concurrent_downloads_have_distinct_temporary_paths_and_cleanup_owns_only_its_path() {
        let folder = std::env::temp_dir().join(format!(
            "SystemMonitorX-download-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&folder).unwrap();
        let first = unique_download_path(&folder, "app.msi");
        let second = unique_download_path(&folder, "app.msi");
        assert_ne!(first, second);
        fs::write(&first, b"incomplete").unwrap();
        fs::write(&second, b"another download").unwrap();
        drop(TemporaryDownload(first.clone()));
        assert!(!first.exists());
        assert_eq!(fs::read(&second).unwrap(), b"another download");
        fs::remove_file(second).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}

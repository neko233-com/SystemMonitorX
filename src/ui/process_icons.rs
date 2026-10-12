//! Bounded asynchronous executable-icon cache. request() does memory/channel
//! work only; one COM worker owns Shell/file/GDI operations. Only request visible
//! rows, not every executable on each metric refresh.
//!
//! SHGetFileInfoW must run in a COM-initialized background thread and returned
//! HICONs must be destroyed:
//! https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shgetfileinfow
//! DIB reads synchronize with GdiFlush:
//! https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-createdibsection

use gpui_kit::{Image, ImageFormat};
use std::{
    collections::HashMap,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{GetLastError, SetLastError, WIN32_ERROR},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, HBITMAP, HDC, HGDIOBJ, SelectObject,
        },
        Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::{
            Shell::{ExtractIconExW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW},
            WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx, HICON},
        },
    },
    core::PCWSTR,
};

const SIDE: usize = 32;
const MAX_ENTRIES: usize = 256;
const MAX_QUEUED: usize = 128;
const SUCCESS_TTL: Duration = Duration::from_secs(300);
const FAILURE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct ExecutableIcon {
    pub image: Arc<Image>,
    pub source_path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Shell failed and the actual executable icon resource supplied the image.
    pub used_embedded_resource: bool,
}

#[derive(Clone, Debug, Default)]
pub struct IconCacheStats {
    pub ready: usize,
    pub failed: usize,
    pub pending: usize,
    pub capacity: usize,
    pub queue_capacity: usize,
    pub workers: usize,
    pub successful_extractions: u64,
    pub completed: u64,
    pub cache_hits: u64,
    pub queue_rejected: u64,
    pub last_error: Option<String>,
}
struct Entry {
    icon: Option<Arc<ExecutableIcon>>,
    pending: bool,
    updated: Instant,
    touched: u64,
}
struct CacheState {
    entries: HashMap<PathBuf, Entry>,
    stats: IconCacheStats,
    clock: u64,
}

/// Clones share one worker and cache. Dropping clients closes its bounded
/// channel; never join a possibly slow Shell extension from the GUI thread.
#[derive(Clone)]
pub struct ProcessIconCache {
    state: Arc<Mutex<CacheState>>,
    sender: SyncSender<PathBuf>,
    sequence: Arc<AtomicU64>,
}
impl Default for ProcessIconCache {
    fn default() -> Self {
        Self::new()
    }
}
impl ProcessIconCache {
    pub fn new() -> Self {
        Self::with_limits(MAX_ENTRIES, MAX_QUEUED)
    }

    pub fn with_limits(capacity: usize, queue_capacity: usize) -> Self {
        let capacity = capacity.clamp(1, MAX_ENTRIES);
        let queue_capacity = queue_capacity.clamp(1, MAX_QUEUED).min(capacity);
        let state = Arc::new(Mutex::new(CacheState {
            entries: HashMap::new(),
            stats: IconCacheStats {
                capacity,
                queue_capacity,
                ..Default::default()
            },
            clock: 0,
        }));
        let sequence = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::sync_channel::<PathBuf>(queue_capacity);
        let worker_state = state.clone();
        let worker_sequence = sequence.clone();
        let started = std::thread::Builder::new()
            .name("executable-icons".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok();
                for path in receiver {
                    let result = if com.is_ok() {
                        extract_icon(&path)
                    } else {
                        Err("图标线程 COM 初始化失败".to_owned())
                    };
                    let mut state = match worker_state.lock() {
                        Ok(state) => state,
                        Err(_) => break,
                    };
                    if let Some(entry) = state.entries.get_mut(&path) {
                        entry.pending = false;
                        entry.updated = Instant::now();
                        entry.icon = result.as_ref().ok().cloned();
                    }
                    state.stats.completed = state.stats.completed.saturating_add(1);
                    if result.is_ok() {
                        state.stats.successful_extractions =
                            state.stats.successful_extractions.saturating_add(1);
                    } else {
                        state.stats.last_error = result.err();
                    }
                    drop(state);
                    worker_sequence.fetch_add(1, Ordering::Release);
                }
                if com.is_ok() {
                    unsafe { CoUninitialize() };
                }
            })
            .is_ok();
        if let Ok(mut state) = state.lock() {
            state.stats.workers = usize::from(started);
        }
        Self {
            state,
            sender,
            sequence,
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    /// Read cache or enqueue. No canonicalize, metadata, disk access, Shell
    /// call, waiting for the worker, or blocking mutex acquisition occurs here.
    /// Success refreshes after five minutes; failures retry after sixty seconds.
    pub fn request(&self, path: &Path) -> Option<Arc<ExecutableIcon>> {
        if !path.is_absolute() {
            return None;
        }
        let mut state = self.state.try_lock().ok()?;
        state.clock = state.clock.saturating_add(1);
        let touched = state.clock;
        let mut previous = None;
        if let Some(entry) = state.entries.get_mut(path) {
            entry.touched = touched;
            let ttl = if entry.icon.is_some() {
                SUCCESS_TTL
            } else {
                FAILURE_TTL
            };
            if entry.pending || entry.updated.elapsed() < ttl {
                let icon = entry.icon.clone();
                if icon.is_some() {
                    state.stats.cache_hits = state.stats.cache_hits.saturating_add(1);
                }
                return icon;
            }
            previous = entry.icon.clone();
            entry.pending = true;
            entry.updated = Instant::now();
        } else {
            if state.entries.len() >= state.stats.capacity {
                let oldest = state
                    .entries
                    .iter()
                    .filter(|(_, entry)| !entry.pending)
                    .min_by_key(|(_, entry)| entry.touched)
                    .map(|(path, _)| path.clone());
                if let Some(oldest) = oldest {
                    state.entries.remove(&oldest);
                } else {
                    state.stats.queue_rejected = state.stats.queue_rejected.saturating_add(1);
                    return None;
                }
            }
            state.entries.insert(
                path.to_path_buf(),
                Entry {
                    icon: None,
                    pending: true,
                    updated: Instant::now(),
                    touched,
                },
            );
        }
        if self.sender.try_send(path.to_path_buf()).is_err() {
            // Channel pressure must not turn an unqueued request into a permanent
            // pending entry. Preserve any old image and permit immediate retry.
            if let Some(entry) = state.entries.get_mut(path) {
                entry.pending = false;
                entry.updated = Instant::now()
                    .checked_sub(SUCCESS_TTL)
                    .unwrap_or_else(Instant::now);
            }
            state.stats.queue_rejected = state.stats.queue_rejected.saturating_add(1);
        }
        previous
    }

    pub fn stats(&self) -> Option<IconCacheStats> {
        let state = self.state.try_lock().ok()?;
        let mut stats = state.stats.clone();
        for entry in state.entries.values() {
            stats.pending += usize::from(entry.pending);
            stats.ready += usize::from(entry.icon.is_some());
            stats.failed += usize::from(!entry.pending && entry.icon.is_none());
        }
        Some(stats)
    }
}

struct OwnedIcon(HICON);
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        let _ = unsafe { DestroyIcon(self.0) };
    }
}
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

fn draw_pixels(icon: HICON, background: u8) -> Result<Vec<u8>, String> {
    let dc = unsafe { CreateCompatibleDC(None) };
    if dc.is_invalid() {
        return Err("无法创建图标内存 DC".into());
    }
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIDE as i32,
            biHeight: -(SIDE as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bitmap = match unsafe {
        CreateDIBSection(Some(dc), &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
    } {
        Ok(bitmap) => bitmap,
        Err(error) => {
            let _ = unsafe { DeleteDC(dc) };
            return Err(error.to_string());
        }
    };
    let previous = unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) };
    if previous.is_invalid() || bits.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        let _ = unsafe { DeleteDC(dc) };
        return Err("无法选择图标像素位图".into());
    }
    let surface = Surface {
        dc,
        bitmap,
        previous,
    };
    // SAFETY: our top-down 32-bit DIB allocates exactly SIDE*SIDE*4 bytes and
    // remains alive through Surface. No other thread accesses it.
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), SIDE * SIDE * 4) };
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[background, background, background, 0]);
    }
    unsafe {
        DrawIconEx(
            surface.dc,
            0,
            0,
            icon,
            SIDE as i32,
            SIDE as i32,
            0,
            None,
            DI_NORMAL,
        )
    }
    .map_err(|error| error.to_string())?;
    if !unsafe { GdiFlush() }.as_bool() {
        return Err("图标 GDI 绘制尚未完成".into());
    }
    Ok(pixels.to_vec())
}

/// Derive alpha by drawing over black/white, also handling mask-based legacy
/// icons. Store straight BGRA for an ICO decoder, not premultiplied channels.
fn transparent_pixels(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut result = black.to_vec();
    for ((output, black), white) in result
        .chunks_exact_mut(4)
        .zip(black.chunks_exact(4))
        .zip(white.chunks_exact(4))
    {
        let difference = (0..3)
            .map(|channel| white[channel].saturating_sub(black[channel]))
            .max()
            .unwrap_or(255);
        let alpha = 255 - difference;
        for channel in 0..3 {
            output[channel] = if alpha == 0 {
                0
            } else {
                ((u16::from(black[channel]) * 255 + u16::from(alpha) / 2) / u16::from(alpha))
                    .min(255) as u8
            };
        }
        output[3] = alpha;
    }
    result
}

/// Small uncompressed ICO: 32-bit BGRA plus AND mask, without another imaging
/// dependency. GPUI decodes this in its image asset pipeline, without Shell IO.
fn encode_ico(pixels: &[u8]) -> Vec<u8> {
    let color_bytes = SIDE * SIDE * 4;
    let mask_bytes = SIDE * (SIDE / 8);
    let mut bytes = Vec::with_capacity(22 + 40 + color_bytes + mask_bytes);
    bytes.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    bytes.extend_from_slice(&[SIDE as u8, SIDE as u8, 0, 0]);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&((40 + color_bytes + mask_bytes) as u32).to_le_bytes());
    bytes.extend_from_slice(&22u32.to_le_bytes());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&(SIDE as i32).to_le_bytes());
    bytes.extend_from_slice(&((SIDE * 2) as i32).to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(color_bytes as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    for y in (0..SIDE).rev() {
        bytes.extend_from_slice(&pixels[y * SIDE * 4..(y + 1) * SIDE * 4]);
    }
    for y in (0..SIDE).rev() {
        let mut row = [0u8; SIDE / 8];
        for x in 0..SIDE {
            if pixels[(y * SIDE + x) * 4 + 3] == 0 {
                row[x / 8] |= 1 << (7 - x % 8);
            }
        }
        bytes.extend_from_slice(&row);
    }
    bytes
}

fn shell_path(path: &Path) -> Result<Vec<u16>, String> {
    let mut units: Vec<_> = path.as_os_str().encode_wide().collect();
    if units.starts_with(&[92, 92, 63, 92]) {
        if units.get(4..8) == Some(&[85, 78, 67, 92]) {
            units = [vec![92, 92], units[8..].to_vec()].concat();
        } else if units.get(5..7) == Some(&[58, 92]) {
            units.drain(..4);
        } else {
            return Err("Shell 不支持此图标命名空间".into());
        }
    }
    if units.len() >= 260 || units.contains(&0) {
        return Err("Shell 图标路径超长或含空字符".into());
    }
    units.push(0);
    Ok(units)
}

fn pixels_from_icon(icon: OwnedIcon) -> Result<Vec<u8>, String> {
    let black = draw_pixels(icon.0, 0)?;
    let white = draw_pixels(icon.0, 255)?;
    let pixels = transparent_pixels(&black, &white);
    if !pixels.chunks_exact(4).any(|pixel| pixel[3] > 0) {
        return Err("图标像素为空".into());
    }
    Ok(pixels)
}

/// Read the first real icon resource directly, without relying on Shell's
/// association/icon-cache service. Every returned HICON is privately owned.
/// https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-extracticonexw
fn embedded_icon(path_units: &[u16]) -> Result<OwnedIcon, String> {
    let mut icon = HICON::default();
    unsafe { SetLastError(WIN32_ERROR(0)) };
    let count = unsafe { ExtractIconExW(PCWSTR(path_units.as_ptr()), 0, Some(&mut icon), None, 1) };
    let error = unsafe { GetLastError() }.0;
    if count == 1 && !icon.is_invalid() {
        return Ok(OwnedIcon(icon));
    }
    if !icon.is_invalid() {
        drop(OwnedIcon(icon));
    }
    Err(format!("ExtractIconExW count={count}, Win32={error}"))
}

fn extract_icon(path: &Path) -> Result<Arc<ExecutableIcon>, String> {
    if !std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("图标来源不是可执行文件".into());
    }
    let path_units = shell_path(path)?;
    let mut info = SHFILEINFOW::default();
    unsafe { SetLastError(WIN32_ERROR(0)) };
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(path_units.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    let shell_error = unsafe { GetLastError() }.0;
    let shell_pixels = if result != 0 && !info.hIcon.is_invalid() {
        pixels_from_icon(OwnedIcon(info.hIcon))
    } else {
        // Even a failed call must not leak an unexpected partially returned icon.
        if !info.hIcon.is_invalid() {
            drop(OwnedIcon(info.hIcon));
        }
        Err(format!(
            "SHGetFileInfoW return={result}, Win32={shell_error}"
        ))
    };
    let (pixels, used_embedded_resource) = match shell_pixels {
        Ok(pixels) => (pixels, false),
        Err(shell_error) => {
            let pixels = embedded_icon(&path_units)
                .and_then(pixels_from_icon)
                .map_err(|error| {
                    // Report codes and a basename only. Full local paths stay in
                    // request/source_path, never in public test/CI diagnostics.
                    format!(
                        "{shell_error}; {error}; file={}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )
                })?;
            (pixels, true)
        }
    };
    Ok(Arc::new(ExecutableIcon {
        image: Arc::new(Image::from_bytes(ImageFormat::Ico, encode_ico(&pixels))),
        source_path: path.to_path_buf(),
        width: SIDE as u32,
        height: SIDE as u32,
        used_embedded_resource,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn statistics(cache: &ProcessIconCache) -> IconCacheStats {
        let started = Instant::now();
        loop {
            if let Some(stats) = cache.stats() {
                return stats;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn wait_icon(cache: &ProcessIconCache, path: &Path) -> Arc<ExecutableIcon> {
        let started = Instant::now();
        loop {
            if let Some(icon) = cache.request(path) {
                return icon;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "real Shell icon failed: {:?}",
                cache.stats()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn real_current_executable_icon_is_nonempty_and_reuses_cached_asset() {
        let cache = ProcessIconCache::new();
        let path = std::env::current_exe().unwrap();
        let first = wait_icon(&cache, &path);
        assert_eq!((first.width, first.height), (32, 32));
        assert_eq!(first.source_path, path);
        assert_eq!(&first.image.bytes[..6], &[0, 0, 1, 0, 1, 0]);
        assert!(first.image.bytes.len() > 4096);
        let second = wait_icon(&cache, &path);
        assert!(Arc::ptr_eq(&first, &second));
        let stats = statistics(&cache);
        assert_eq!(stats.workers, 1);
        assert_eq!(stats.successful_extractions, 1);
        assert_eq!(stats.ready, 1);
        eprintln!(
            "real executable icon: pixels=32x32 encoded_bytes={} cached_asset_reused=true",
            first.image.bytes.len()
        );
    }

    #[test]
    fn real_current_executable_embedded_resource_is_a_valid_fallback() {
        let path = std::env::current_exe().unwrap();
        let icon = embedded_icon(&shell_path(&path).unwrap())
            .expect("the application test executable must retain its real app.ico resource");
        let pixels = pixels_from_icon(icon).unwrap();
        assert_eq!(pixels.len(), SIDE * SIDE * 4);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    #[test]
    fn cache_boundaries_hold_with_real_windows_executables() {
        let cache = ProcessIconCache::with_limits(2, 2);
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        // Some system binaries have no embedded icon (for example rundll32).
        // Capacity success cases use files with real icon resources; failure
        // caching is covered separately below without assuming Shell services.
        for path in [
            std::env::current_exe().unwrap(),
            system.join("cmd.exe"),
            system.join("msiexec.exe"),
        ] {
            wait_icon(&cache, &path);
            let stats = statistics(&cache);
            assert!(stats.ready + stats.failed <= 2);
            assert!(stats.pending <= 2);
            assert_eq!(stats.capacity, 2);
            assert_eq!(stats.workers, 1);
        }
        assert_eq!(statistics(&cache).ready, 2);
        assert!(cache.request(Path::new("relative.exe")).is_none());
    }

    #[test]
    fn expiry_requeues_real_icon_and_negative_cache_without_disk_in_request() {
        let cache = ProcessIconCache::with_limits(3, 2);
        let path = std::env::current_exe().unwrap();
        wait_icon(&cache, &path);
        {
            let mut state = cache.state.lock().unwrap();
            state.entries.get_mut(&path).unwrap().updated =
                Instant::now() - SUCCESS_TTL - Duration::from_secs(1);
        }
        cache.request(&path);
        let started = Instant::now();
        while statistics(&cache).completed < 2 {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        let missing = std::env::temp_dir().join(format!(
            "SystemMonitorX-no-icon-{}-not-created.exe",
            std::process::id()
        ));
        cache.request(&missing);
        let started = Instant::now();
        while statistics(&cache).failed == 0 {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        let before = statistics(&cache).completed;
        for _ in 0..10 {
            assert!(cache.request(&missing).is_none());
        }
        assert_eq!(statistics(&cache).completed, before);
        assert_eq!(statistics(&cache).pending, 0);
    }

    #[test]
    fn encoded_ico_preserves_orientation_mask_and_legacy_transparency() {
        let mut black = vec![0; SIDE * SIDE * 4];
        let mut white = vec![255; SIDE * SIDE * 4];
        black[..4].copy_from_slice(&[0, 0, 255, 0]);
        white[..4].copy_from_slice(&[0, 0, 255, 0]);
        let pixels = transparent_pixels(&black, &white);
        assert_eq!(&pixels[..4], &[0, 0, 255, 255]);
        assert_eq!(&pixels[4..8], &[0, 0, 0, 0]);
        let encoded = encode_ico(&pixels);
        let first_top_pixel = 22 + 40 + (SIDE - 1) * SIDE * 4;
        assert_eq!(
            &encoded[first_top_pixel..first_top_pixel + 4],
            &[0, 0, 255, 255]
        );
        let top_mask = 22 + 40 + SIDE * SIDE * 4 + (SIDE - 1) * (SIDE / 8);
        assert_eq!(encoded[top_mask], 0b0111_1111);
    }
}

//! Bulk native process counters, bounded history, and a real collector benchmark.
//!
//! Process counters come from one SystemProcessInformation query. Static paths
//! and token identities are cached per (PID, creation time); account resolution is
//! additionally cached per SID. History uses differences of cumulative counters,
//! never an integration of rounded percentages or displayed throughput.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use ntapi::ntexapi::{
    NtQuerySystemInformation, SYSTEM_PROCESS_INFORMATION, SYSTEM_THREAD_INFORMATION,
    SystemProcessInformation,
};
use ntapi::ntkeapi::{Suspended, Waiting, WrSuspended};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Security::{
    GetLengthSid, GetTokenInformation, IsValidSid, LookupAccountSidW, SID_NAME_USE, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::monitor::{Monitor, ProcessSample};
use crate::network_process::ProcessNetworkSnapshot;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time_unix_secs: u64,
    #[serde(default)]
    pub start_time_windows_ticks: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct NativeProcessSample {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub start_time_unix_secs: u64,
    pub start_time_windows_ticks: Option<u64>,
    pub cpu_ticks: Option<u64>,
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    pub working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub private_working_set_bytes: Option<u64>,
    pub virtual_bytes: Option<u64>,
    pub thread_count: Option<u32>,
    pub handle_count: Option<u32>,
    pub session_id: Option<u32>,
    pub exe: Option<PathBuf>,
    pub user_name: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug)]
struct StaticProcessInfo {
    exe: Option<PathBuf>,
    user_name: Option<String>,
}

pub struct ProcessPerformanceCollector {
    buffer: Vec<usize>,
    path_buffer: Vec<u16>,
    token_buffer: Vec<usize>,
    static_processes: HashMap<ProcessIdentity, StaticProcessInfo>,
    account_names: HashMap<Vec<u8>, Option<String>>,
    sampled_at: Instant,
}

impl Default for ProcessPerformanceCollector {
    fn default() -> Self {
        Self {
            buffer: vec![0usize; (512 * 1024) / size_of::<usize>()],
            path_buffer: vec![0u16; 4096],
            token_buffer: vec![0usize; 512 / size_of::<usize>()],
            static_processes: HashMap::new(),
            account_names: HashMap::new(),
            sampled_at: Instant::now(),
        }
    }
}

impl ProcessPerformanceCollector {
    pub fn sampled_at(&self) -> Instant {
        self.sampled_at
    }

    pub fn sample(&mut self) -> Result<Vec<NativeProcessSample>> {
        const LENGTH_MISMATCH: i32 = 0xc0000004u32 as i32;
        let mut returned = 0u32;
        let mut completed = false;
        for _ in 0..6 {
            let bytes = self.buffer.len() * size_of::<usize>();
            // SAFETY: Vec<usize> supplies alignment and initialized writable storage.
            // This info class contains only a process snapshot and does not mutate processes.
            let status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    self.buffer.as_mut_ptr().cast(),
                    bytes as u32,
                    &mut returned,
                )
            };
            if status >= 0 {
                completed = true;
                break;
            }
            if status != LENGTH_MISMATCH {
                bail!(
                    "Native process query failed: NTSTATUS 0x{:08x}",
                    status as u32
                );
            }
            let needed = (returned as usize)
                .max(bytes.saturating_mul(2))
                .saturating_add(64 * 1024);
            ensure!(
                needed <= 64 * 1024 * 1024,
                "Native process snapshot exceeds the 64 MiB safety limit"
            );
            self.buffer.resize(needed.div_ceil(size_of::<usize>()), 0);
        }
        ensure!(
            completed,
            "Native process list changed too quickly to collect"
        );
        self.sampled_at = Instant::now();
        let available = self.buffer.len() * size_of::<usize>();
        let used = returned as usize;
        ensure!(
            used > 0 && used <= available,
            "Native process snapshot returned an invalid size"
        );
        let header_size = offset_of!(SYSTEM_PROCESS_INFORMATION, Threads);
        let base = self.buffer.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        let mut samples = Vec::with_capacity(self.static_processes.len().max(512));
        loop {
            ensure!(
                offset
                    .checked_add(header_size)
                    .is_some_and(|end| end <= used)
                    && offset
                        .checked_add(size_of::<SYSTEM_PROCESS_INFORMATION>())
                        .is_some_and(|end| end <= available)
                    && offset.is_multiple_of(std::mem::align_of::<SYSTEM_PROCESS_INFORMATION>()),
                "Native process record is truncated or unaligned"
            );
            // SAFETY: the header and typed storage bounds/alignment were verified.
            // Threads are not read; their trailing flexible array is not enumerated.
            let record = unsafe { &*base.add(offset).cast::<SYSTEM_PROCESS_INFORMATION>() };
            let thread_bytes = (record.NumberOfThreads as usize)
                .checked_mul(size_of::<SYSTEM_THREAD_INFORMATION>())
                .context("Native thread list size overflow")?;
            let record_end = if record.NextEntryOffset > 0 {
                offset
                    .checked_add(record.NextEntryOffset as usize)
                    .context("Native record size overflow")?
            } else {
                used
            };
            ensure!(
                offset
                    .checked_add(header_size)
                    .and_then(|start| start.checked_add(thread_bytes))
                    .is_some_and(|end| end <= record_end && end <= used),
                "Native thread array exceeds its process record"
            );
            // SAFETY: the full, properly aligned thread array lies in this record.
            let threads = unsafe {
                std::slice::from_raw_parts(
                    base.add(offset + header_size)
                        .cast::<SYSTEM_THREAD_INFORMATION>(),
                    record.NumberOfThreads as usize,
                )
            };
            let status = if threads.is_empty() {
                "正在退出"
            } else if threads.iter().all(|thread| {
                thread.ThreadState == Waiting
                    && (thread.WaitReason == Suspended || thread.WaitReason == WrSuspended)
            }) {
                "已挂起"
            } else {
                "运行中"
            };
            let pid =
                u32::try_from(record.UniqueProcessId as usize).context("Native PID exceeds u32")?;
            let created = unsafe { *record.CreateTime.QuadPart() };
            let kernel = unsafe { *record.KernelTime.QuadPart() };
            let user = unsafe { *record.UserTime.QuadPart() };
            let name = if record.ImageName.Length == 0 {
                match pid {
                    0 => "System Idle Process".into(),
                    4 => "System".into(),
                    _ => format!("Process {pid}"),
                }
            } else {
                let pointer = record.ImageName.Buffer as usize;
                let length = record.ImageName.Length as usize;
                let begin = base as usize;
                ensure!(
                    length.is_multiple_of(2)
                        && pointer >= begin
                        && pointer.is_multiple_of(2)
                        && pointer
                            .checked_add(length)
                            .is_some_and(|end| end <= begin + used),
                    "Native process name lies outside its snapshot"
                );
                // SAFETY: UTF-16 pointer alignment and full byte range were checked.
                String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(record.ImageName.Buffer, length / 2)
                })
            };
            samples.push(NativeProcessSample {
                pid,
                parent_pid: u32::try_from(record.InheritedFromUniqueProcessId as usize)
                    .ok()
                    .filter(|parent| *parent != 0),
                name,
                start_time_unix_secs: windows_ticks_to_unix(created),
                start_time_windows_ticks: u64::try_from(created).ok().filter(|ticks| *ticks > 0),
                cpu_ticks: u64::try_from(kernel)
                    .ok()
                    .zip(u64::try_from(user).ok())
                    .and_then(|(kernel, user)| kernel.checked_add(user)),
                read_bytes: u64::try_from(unsafe { *record.ReadTransferCount.QuadPart() }).ok(),
                write_bytes: u64::try_from(unsafe { *record.WriteTransferCount.QuadPart() }).ok(),
                working_set_bytes: Some(record.WorkingSetSize as u64),
                private_bytes: Some(record.PrivatePageCount as u64),
                private_working_set_bytes: u64::try_from(unsafe {
                    *record.WorkingSetPrivateSize.QuadPart()
                })
                .ok(),
                virtual_bytes: Some(record.VirtualSize as u64),
                thread_count: Some(record.NumberOfThreads),
                handle_count: Some(record.HandleCount),
                session_id: Some(record.SessionId),
                status: Some(status.to_owned()),
                ..Default::default()
            });
            if record.NextEntryOffset == 0 {
                break;
            }
            let next = record.NextEntryOffset as usize;
            ensure!(
                next >= header_size,
                "Native process list contains an invalid offset"
            );
            offset = offset
                .checked_add(next)
                .context("Native process offset overflow")?;
        }

        let mut live = HashSet::with_capacity(samples.len());
        for sample in &mut samples {
            let identity = ProcessIdentity {
                pid: sample.pid,
                start_time_unix_secs: sample.start_time_unix_secs,
                start_time_windows_ticks: sample.start_time_windows_ticks,
            };
            live.insert(identity);
            if !self.static_processes.contains_key(&identity) {
                let info = self.static_info(sample.pid, sample.start_time_windows_ticks);
                self.static_processes.insert(identity, info);
            }
            if let Some(info) = self.static_processes.get(&identity) {
                sample.exe.clone_from(&info.exe);
                sample.user_name.clone_from(&info.user_name);
            }
        }
        self.static_processes
            .retain(|identity, _| live.contains(identity));
        // Bound stale SID entries without repeating per-process account resolution.
        if self.account_names.len() > 4096 {
            self.account_names.clear();
        }
        Ok(samples)
    }

    fn static_info(&mut self, pid: u32, expected_created: Option<u64>) -> StaticProcessInfo {
        let empty = StaticProcessInfo {
            exe: None,
            user_name: None,
        };
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return empty;
        };
        let handle = ProcessHandle(handle);
        if let Some(expected_created) = expected_created {
            let (mut created, mut exited, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            if unsafe {
                GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user)
            }
            .is_err()
                || ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
                    != expected_created
            {
                return empty;
            }
        }
        let exe = query_executable(handle.0, &mut self.path_buffer);
        let user_name = self.process_user(handle.0);
        StaticProcessInfo { exe, user_name }
    }

    fn process_user(&mut self, process: HANDLE) -> Option<String> {
        let mut raw = HANDLE::default();
        // SAFETY: process is an owned query handle and token output is valid.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) }.ok()?;
        let token = ProcessHandle(raw);
        let mut required = 0;
        let mut bytes = self.token_buffer.len() * size_of::<usize>();
        let result = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(self.token_buffer.as_mut_ptr().cast()),
                bytes as u32,
                &mut required,
            )
        };
        if result.is_err() {
            if required as usize <= bytes || required > 65_536 {
                return None;
            }
            self.token_buffer
                .resize((required as usize).div_ceil(size_of::<usize>()), 0);
            bytes = self.token_buffer.len() * size_of::<usize>();
            unsafe {
                GetTokenInformation(
                    token.0,
                    TokenUser,
                    Some(self.token_buffer.as_mut_ptr().cast()),
                    bytes as u32,
                    &mut required,
                )
            }
            .ok()?;
        }
        if required < size_of::<TOKEN_USER>() as u32 {
            return None;
        }
        // SAFETY: the aligned initialized buffer contains TOKEN_USER after success.
        let user = unsafe { &*self.token_buffer.as_ptr().cast::<TOKEN_USER>() };
        let start = self.token_buffer.as_ptr() as usize;
        let sid = user.User.Sid;
        let pointer = sid.0 as usize;
        if pointer < start
            || pointer
                .checked_add(8)
                .is_none_or(|end| end > start + required as usize)
        {
            return None;
        }
        // A SID header contains a subauthority count. Bound its declared body
        // before any Windows validation routine can read beyond the header.
        let declared_length = 8usize + 4 * unsafe { *sid.0.cast::<u8>().add(1) } as usize;
        if pointer
            .checked_add(declared_length)
            .is_none_or(|end| end > start + required as usize)
        {
            return None;
        }
        // SAFETY: SID is a token-owned pointer within a successfully filled buffer.
        if !unsafe { IsValidSid(sid) }.as_bool() {
            return None;
        }
        let length = unsafe { GetLengthSid(sid) } as usize;
        if pointer
            .checked_add(length)
            .is_none_or(|end| end > start + required as usize)
        {
            return None;
        }
        let key = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), length) }.to_vec();
        if let Some(cached) = self.account_names.get(&key) {
            return cached.clone();
        }
        let mut name = vec![0u16; 256];
        let mut domain = vec![0u16; 256];
        let mut name_len = name.len() as u32;
        let mut domain_len = domain.len() as u32;
        let mut usage = SID_NAME_USE::default();
        let mut result = unsafe {
            LookupAccountSidW(
                PCWSTR::null(),
                sid,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                Some(PWSTR(domain.as_mut_ptr())),
                &mut domain_len,
                &mut usage,
            )
        };
        if result.is_err()
            && name_len <= 16_384
            && domain_len <= 16_384
            && (name_len > 256 || domain_len > 256)
        {
            name.resize(name_len as usize, 0);
            domain.resize(domain_len as usize, 0);
            result = unsafe {
                LookupAccountSidW(
                    PCWSTR::null(),
                    sid,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    Some(PWSTR(domain.as_mut_ptr())),
                    &mut domain_len,
                    &mut usage,
                )
            };
        }
        let resolved = result.ok().and_then(|()| {
            let name_units = name.get(..name_len as usize)?;
            let domain_units = domain.get(..domain_len as usize)?;
            let name = String::from_utf16_lossy(
                &name_units[..name_units
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(name_units.len())],
            );
            let domain = String::from_utf16_lossy(
                &domain_units[..domain_units
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(domain_units.len())],
            );
            if name.is_empty() {
                None
            } else if domain.is_empty() {
                Some(name)
            } else {
                Some(format!("{domain}\\{name}"))
            }
        });
        self.account_names.insert(key, resolved.clone());
        resolved
    }
}

struct ProcessHandle(HANDLE);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn query_executable(process: HANDLE, buffer: &mut Vec<u16>) -> Option<PathBuf> {
    let mut length = buffer.len() as u32;
    let mut result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    if result.is_err() && buffer.len() < 32_768 {
        buffer.resize(32_768, 0);
        length = buffer.len() as u32;
        result = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
    }
    result.ok()?;
    let text = String::from_utf16_lossy(buffer.get(..length as usize)?);
    (!text.is_empty()).then(|| PathBuf::from(text))
}

fn windows_ticks_to_unix(ticks: i64) -> u64 {
    u64::try_from(ticks)
        .unwrap_or(0)
        .saturating_div(10_000_000)
        .saturating_sub(11_644_473_600)
}

#[derive(Clone, Debug, Default)]
pub struct SystemPerformanceSample {
    pub handle_count: Option<u32>,
    pub thread_count: Option<u32>,
    pub process_count: Option<u32>,
    pub committed_bytes: Option<u64>,
    pub commit_limit_bytes: Option<u64>,
    pub commit_peak_bytes: Option<u64>,
    pub cached_memory_bytes: Option<u64>,
    pub kernel_paged_bytes: Option<u64>,
    pub kernel_nonpaged_bytes: Option<u64>,
}

pub fn system_performance() -> SystemPerformanceSample {
    let mut counters = PERFORMANCE_INFORMATION {
        cb: size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: the buffer and supplied size are exactly PERFORMANCE_INFORMATION.
    let buffer_size = counters.cb;
    if unsafe { GetPerformanceInfo(&mut counters, buffer_size) }.is_err() {
        return SystemPerformanceSample::default();
    }
    let bytes = |pages: usize| (pages as u64).checked_mul(counters.PageSize as u64);
    SystemPerformanceSample {
        handle_count: Some(counters.HandleCount),
        thread_count: Some(counters.ThreadCount),
        process_count: Some(counters.ProcessCount),
        committed_bytes: bytes(counters.CommitTotal),
        commit_limit_bytes: bytes(counters.CommitLimit),
        commit_peak_bytes: bytes(counters.CommitPeak),
        cached_memory_bytes: bytes(counters.SystemCache),
        kernel_paged_bytes: bytes(counters.KernelPaged),
        kernel_nonpaged_bytes: bytes(counters.KernelNonpaged),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessHistorySample {
    pub identity: ProcessIdentity,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub first_seen_unix_secs: u64,
    pub last_seen_unix_secs: u64,
    pub cpu_seconds: Option<f64>,
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    /// Actual bytes from complete ETW intervals; never reconstructed from rates.
    #[serde(default)]
    pub network_received_bytes: Option<u64>,
    #[serde(default)]
    pub network_transmitted_bytes: Option<u64>,
    #[serde(default)]
    pub network_covered_seconds: Option<f64>,
    /// Sampling gaps survive restarts so partial totals remain clearly labeled.
    #[serde(default)]
    pub network_has_gaps: bool,
    pub running: bool,
    first_cpu_ticks: Option<u64>,
    last_cpu_ticks: Option<u64>,
    first_read_bytes: Option<u64>,
    first_write_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AppHistorySample {
    pub name: String,
    pub exe: Option<PathBuf>,
    pub process_instances: usize,
    pub running_instances: usize,
    pub first_seen_unix_secs: u64,
    pub last_seen_unix_secs: u64,
    pub cpu_seconds: Option<f64>,
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    pub network_received_bytes: Option<u64>,
    pub network_transmitted_bytes: Option<u64>,
    /// Summed valid process intervals. Concurrent processes overlap; this is
    /// process-seconds, not wall-clock application uptime or daily coverage.
    pub network_covered_seconds: Option<f64>,
    pub network_has_gaps: bool,
    pub network_note: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    entries: Vec<ProcessHistorySample>,
}

pub struct HistoryStore {
    entries: HashMap<ProcessIdentity, ProcessHistorySample>,
    path: Option<PathBuf>,
    last_saved: Instant,
    dirty: bool,
    write_blocked: bool,
    last_network_interval_end: Option<Instant>,
    pub note: Option<String>,
    pub network_note: Option<String>,
}

impl HistoryStore {
    pub fn open_default() -> Self {
        let path = std::env::var_os("LOCALAPPDATA").map(|root| {
            PathBuf::from(root)
                .join("SystemMonitorX")
                .join("history-v1.json")
        });
        Self::open(path)
    }

    /// None creates an in-memory history, useful for reproducible performance probes.
    pub fn open(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            entries: HashMap::new(),
            path,
            last_saved: Instant::now(),
            dirty: false,
            write_blocked: false,
            last_network_interval_end: None,
            note: None,
            network_note: None,
        };
        if let Some(path) = &store.path {
            if path.exists() {
                match read_history(path) {
                    Ok(entries) => {
                        store.entries = entries
                            .into_iter()
                            .map(|mut entry| {
                                entry.running = false;
                                (entry.identity, entry)
                            })
                            .collect();
                    }
                    Err(error) => {
                        store.note = Some(format!("历史记录无法读取：{error:#}"));
                        // Preserve an unreadable/versioned user file for recovery.
                        store.write_blocked = true;
                    }
                }
            }
        } else {
            store.note = Some("历史记录保存在本次会话内。".into());
        }
        store
    }

    pub fn update(&mut self, processes: &[ProcessSample]) {
        let now = unix_now();
        for entry in self.entries.values_mut() {
            entry.running = false;
        }
        for process in processes
            .iter()
            .filter(|process| process.pid != 0 && process.start_time_unix_secs > 0)
        {
            let identity = ProcessIdentity {
                pid: process.pid,
                start_time_unix_secs: process.start_time_unix_secs,
                start_time_windows_ticks: process.start_time_windows_ticks,
            };
            let ticks = process.cpu_time_ticks.or_else(|| {
                process.cpu_time_seconds.and_then(|seconds| {
                    (seconds.is_finite()
                        && seconds >= 0.0
                        && seconds <= u64::MAX as f64 / 10_000_000.0)
                        .then(|| (seconds * 10_000_000.0).round() as u64)
                })
            });
            let entry = self
                .entries
                .entry(identity)
                .or_insert_with(|| ProcessHistorySample {
                    identity,
                    name: process.name.clone(),
                    exe: process.exe.clone(),
                    first_seen_unix_secs: now,
                    last_seen_unix_secs: now,
                    cpu_seconds: ticks.map(|_| 0.0),
                    read_bytes: process.total_disk_read_bytes.map(|_| 0),
                    write_bytes: process.total_disk_write_bytes.map(|_| 0),
                    network_received_bytes: None,
                    network_transmitted_bytes: None,
                    network_covered_seconds: None,
                    network_has_gaps: false,
                    running: true,
                    first_cpu_ticks: ticks,
                    last_cpu_ticks: ticks,
                    first_read_bytes: process.total_disk_read_bytes,
                    first_write_bytes: process.total_disk_write_bytes,
                });
            entry.last_seen_unix_secs = now;
            entry.running = true;
            if entry.exe.is_none() {
                entry.exe.clone_from(&process.exe);
            }
            if entry.first_cpu_ticks.is_none() {
                entry.first_cpu_ticks = ticks;
            }
            if entry.first_read_bytes.is_none() {
                entry.first_read_bytes = process.total_disk_read_bytes;
            }
            if entry.first_write_bytes.is_none() {
                entry.first_write_bytes = process.total_disk_write_bytes;
            }
            if let Some(ticks) = ticks {
                entry.last_cpu_ticks = Some(ticks);
                entry.cpu_seconds = entry
                    .first_cpu_ticks
                    .and_then(|first| ticks.checked_sub(first))
                    .map(|ticks| ticks as f64 / 10_000_000.0);
            }
            if let Some(current) = process.total_disk_read_bytes {
                entry.read_bytes = entry
                    .first_read_bytes
                    .and_then(|first| current.checked_sub(first));
            }
            if let Some(current) = process.total_disk_write_bytes {
                entry.write_bytes = entry
                    .first_write_bytes
                    .and_then(|first| current.checked_sub(first));
            }
        }
        // Retain at most 2048 identities, oldest last-observed records first.
        if self.entries.len() > 2048 {
            let mut ages: Vec<_> = self
                .entries
                .values()
                .map(|entry| (entry.last_seen_unix_secs, entry.identity))
                .collect();
            ages.sort_unstable();
            for (_, identity) in ages.into_iter().take(self.entries.len() - 2048) {
                self.entries.remove(&identity);
            }
        }
        self.dirty = true;
        if self.last_saved.elapsed() >= Duration::from_secs(60)
            && let Err(error) = self.persist()
        {
            self.note = Some(format!("历史记录无法保存：{error:#}"));
        }
    }

    /// Add only actual packet bytes delivered in a complete ETW interval.
    /// Call after update() with the same process snapshot. New/missing identities,
    /// warm-up, permission errors and loss intervals are gaps, not zero traffic.
    /// The monotonic interval end rejects duplicate or out-of-order snapshots.
    pub fn observe_network(&mut self, snapshot: &ProcessNetworkSnapshot) {
        let Some(interval_end) = snapshot.interval_end else {
            self.mark_network_gap();
            self.network_note = Some("网络历史缺少可验证的完整采样周期，未计入流量。".into());
            return;
        };
        if self
            .last_network_interval_end
            .is_some_and(|last| interval_end <= last)
        {
            return;
        }
        self.last_network_interval_end = Some(interval_end);
        self.network_note = Some(format!(
            "网络仅累计实际监测中完整 ETW 周期的收发字节；覆盖秒数为有效进程秒，并发进程的时间相加。{}",
            snapshot.status
        ));
        let complete = snapshot.available
            && snapshot.complete_interval
            && snapshot.elapsed_seconds.is_finite()
            && snapshot.elapsed_seconds > 0.0;
        for entry in self.entries.values_mut().filter(|entry| entry.running) {
            let rate = snapshot.rates.get(&entry.identity.pid).filter(|rate| {
                rate.process_start_time_windows_ticks >= 116_444_736_000_000_000
                    && entry.identity.start_time_windows_ticks
                        == Some(rate.process_start_time_windows_ticks)
            });
            let Some(rate) = rate.filter(|_| complete) else {
                entry.network_has_gaps = true;
                continue;
            };
            let received = entry
                .network_received_bytes
                .unwrap_or(0)
                .checked_add(rate.received_bytes);
            let transmitted = entry
                .network_transmitted_bytes
                .unwrap_or(0)
                .checked_add(rate.sent_bytes);
            let covered = entry.network_covered_seconds.unwrap_or(0.0) + snapshot.elapsed_seconds;
            if let (Some(received), Some(transmitted)) = (received, transmitted)
                && covered.is_finite()
            {
                entry.network_received_bytes = Some(received);
                entry.network_transmitted_bytes = Some(transmitted);
                entry.network_covered_seconds = Some(covered);
            } else {
                // Keep the previous valid partial totals if a counter overflows.
                entry.network_has_gaps = true;
            }
        }
        self.dirty = true;
    }

    fn mark_network_gap(&mut self) {
        for entry in self.entries.values_mut().filter(|entry| entry.running) {
            entry.network_has_gaps = true;
        }
        self.dirty = true;
    }

    pub fn process_history(&self) -> Vec<ProcessHistorySample> {
        let mut entries: Vec<_> = self.entries.values().cloned().collect();
        entries.sort_by(|left, right| {
            right
                .last_seen_unix_secs
                .cmp(&left.last_seen_unix_secs)
                .then_with(|| left.identity.cmp(&right.identity))
        });
        entries
    }

    pub fn app_history(&self) -> Vec<AppHistorySample> {
        let mut apps: BTreeMap<String, AppHistorySample> = BTreeMap::new();
        // Up to 2048 u64 instance totals fit in u128. An overflowing app total
        // becomes unavailable instead of wrapping or displaying a made-up cap.
        let mut network_totals: HashMap<String, (Option<u128>, Option<u128>)> = HashMap::new();
        for entry in self.entries.values() {
            let key = entry
                .exe
                .as_ref()
                .map(|path| path.to_string_lossy().to_lowercase())
                .unwrap_or_else(|| format!("name:{}", entry.name.to_lowercase()));
            let app = apps.entry(key.clone()).or_insert_with(|| AppHistorySample {
                name: entry.name.clone(),
                exe: entry.exe.clone(),
                first_seen_unix_secs: entry.first_seen_unix_secs,
                ..Default::default()
            });
            app.process_instances += 1;
            app.running_instances += usize::from(entry.running);
            app.first_seen_unix_secs = app.first_seen_unix_secs.min(entry.first_seen_unix_secs);
            app.last_seen_unix_secs = app.last_seen_unix_secs.max(entry.last_seen_unix_secs);
            if let Some(seconds) = entry.cpu_seconds {
                app.cpu_seconds = Some(app.cpu_seconds.unwrap_or(0.0) + seconds);
            }
            if let Some(bytes) = entry.read_bytes {
                app.read_bytes = Some(app.read_bytes.unwrap_or(0).saturating_add(bytes));
            }
            if let Some(bytes) = entry.write_bytes {
                app.write_bytes = Some(app.write_bytes.unwrap_or(0).saturating_add(bytes));
            }
            let totals = network_totals.entry(key).or_default();
            if let Some(bytes) = entry.network_received_bytes {
                totals.0 = Some(totals.0.unwrap_or(0) + u128::from(bytes));
                app.network_received_bytes = totals.0.and_then(|total| u64::try_from(total).ok());
            }
            if let Some(bytes) = entry.network_transmitted_bytes {
                totals.1 = Some(totals.1.unwrap_or(0) + u128::from(bytes));
                app.network_transmitted_bytes =
                    totals.1.and_then(|total| u64::try_from(total).ok());
            }
            if let Some(seconds) = entry.network_covered_seconds {
                app.network_covered_seconds =
                    Some(app.network_covered_seconds.unwrap_or(0.0) + seconds);
            }
            app.network_has_gaps |= entry.network_has_gaps;
        }
        let mut apps: Vec<_> = apps.into_values().collect();
        for app in &mut apps {
            app.network_note = Some(if app.network_covered_seconds.is_none() {
                "没有完整有效的网络采样周期；权限不足、ETW 未就绪或缺少精确身份时保持不可用。"
                    .into()
            } else if app.network_has_gaps {
                "仅含实际监测的有效 ETW 周期，未覆盖的周期不计；覆盖为进程秒，并发进程时间相加。"
                    .into()
            } else {
                "仅含实际监测的完整 ETW 周期，不代表全天或进程全生命周期流量；覆盖为进程秒。".into()
            });
        }
        apps.sort_by(|left, right| {
            right
                .cpu_seconds
                .unwrap_or(0.0)
                .total_cmp(&left.cpu_seconds.unwrap_or(0.0))
                .then_with(|| left.name.cmp(&right.name))
        });
        apps
    }

    pub fn persist(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        ensure!(
            !self.write_blocked,
            "保留无法读取的历史文件；请明确重置历史后再保存"
        );
        self.last_saved = Instant::now();
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().context("历史文件缺少父目录")?;
        std::fs::create_dir_all(parent)?;
        let file = HistoryFile {
            version: 1,
            entries: self.process_history(),
        };
        let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec(&file)?)?;
        std::fs::rename(&temporary, path)?;
        self.dirty = false;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        self.entries.clear();
        self.write_blocked = false;
        self.last_network_interval_end = None;
        self.dirty = true;
        self.note = None;
        self.network_note = None;
        self.persist()
    }
}

impl Drop for HistoryStore {
    fn drop(&mut self) {
        let _ = self.persist();
    }
}

fn read_history(path: &Path) -> Result<Vec<ProcessHistorySample>> {
    ensure!(
        std::fs::metadata(path)?.len() <= 8 * 1024 * 1024,
        "历史文件超过 8 MiB 限制"
    );
    let file: HistoryFile = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(file.version == 1, "历史文件版本无法识别");
    let mut entries = file.entries;
    entries.retain(|entry| {
        entry.identity.pid != 0
            && entry.identity.start_time_unix_secs > 0
            && entry
                .cpu_seconds
                .is_none_or(|value| value.is_finite() && value >= 0.0)
            && entry
                .network_covered_seconds
                .is_none_or(|value| value.is_finite() && (0.0..=3_155_760_000.0).contains(&value))
    });
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_seen_unix_secs));
    entries.truncate(2048);
    Ok(entries)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkReport {
    pub samples: usize,
    pub cadence_ms: u128,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub self_cpu_seconds: f64,
    pub wall_seconds: f64,
    pub self_cpu_percent_machine: f64,
    pub self_memory_bytes: Option<u64>,
    pub process_count: usize,
    pub logical_cpus: usize,
    pub gpu_count: usize,
    pub disk_count: usize,
    pub network_count: usize,
    pub raw_latencies_ms: Vec<f64>,
}

pub fn run_benchmark(
    monitor: &mut Monitor,
    samples: usize,
    cadence: Duration,
) -> Result<BenchmarkReport> {
    ensure!(
        (2..=10_000).contains(&samples),
        "Benchmark sample count must be 2..10000"
    );
    ensure!(
        cadence >= Duration::from_millis(200),
        "CPU sampling cadence must be at least 200 ms"
    );
    monitor.sample();
    let start_cpu = current_cpu_seconds()?;
    let start = Instant::now();
    let mut timings = Vec::with_capacity(samples);
    let mut last = None;
    for _ in 0..samples {
        std::thread::sleep(cadence);
        let stamp = Instant::now();
        last = Some(monitor.sample());
        timings.push(stamp.elapsed().as_secs_f64() * 1000.0);
    }
    let wall_seconds = start.elapsed().as_secs_f64();
    let cpu_seconds = (current_cpu_seconds()? - start_cpu).max(0.0);
    let snapshot = last.context("No benchmark sample was collected")?;
    let mut sorted = timings.clone();
    sorted.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        sorted[((sorted.len() as f64 * fraction).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1)]
    };
    Ok(BenchmarkReport {
        samples,
        cadence_ms: cadence.as_millis(),
        p50_ms: percentile(0.5),
        p95_ms: percentile(0.95),
        max_ms: sorted[sorted.len() - 1],
        self_cpu_seconds: cpu_seconds,
        wall_seconds,
        self_cpu_percent_machine: cpu_seconds / wall_seconds / snapshot.logical_cpus.max(1) as f64
            * 100.0,
        self_memory_bytes: snapshot
            .processes
            .iter()
            .find(|process| process.pid == std::process::id())
            .and_then(|process| process.working_set_bytes),
        process_count: snapshot.processes.len(),
        logical_cpus: snapshot.logical_cpus,
        gpu_count: snapshot.gpus.len(),
        disk_count: snapshot.disks.len(),
        network_count: snapshot.networks.len(),
        raw_latencies_ms: timings,
    })
}

fn current_cpu_seconds() -> Result<f64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }?;
    let ticks =
        |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Ok(ticks(kernel).saturating_add(ticks(user)) as f64 / 10_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_process::ProcessNetworkRate;

    fn network_process_fixture(offset_ticks: u64) -> ProcessSample {
        ProcessSample {
            pid: 123,
            name: "network-fixture".into(),
            start_time_unix_secs: 100,
            start_time_windows_ticks: Some(116_444_737_000_000_000 + offset_ticks),
            ..Default::default()
        }
    }

    fn network_interval(
        process: &ProcessSample,
        received: u64,
        sent: u64,
        seconds: f64,
        end: Instant,
    ) -> ProcessNetworkSnapshot {
        ProcessNetworkSnapshot {
            rates: HashMap::from([(
                process.pid,
                ProcessNetworkRate {
                    process_start_time_windows_ticks: process.start_time_windows_ticks.unwrap_or(0),
                    // Deliberately unrelated display rates: history must use bytes.
                    received_bytes_per_sec: 99_000_000.0,
                    sent_bytes_per_sec: 88_000_000.0,
                    received_bytes: received,
                    sent_bytes: sent,
                },
            )]),
            available: true,
            complete_interval: true,
            interval_end: Some(end),
            elapsed_seconds: seconds,
            status: "完整有效的网络周期".into(),
            ..Default::default()
        }
    }

    fn history_fixture_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "systemmonitorx-{label}-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ))
    }

    #[test]
    fn network_history_uses_exact_bytes_and_rejects_duplicate_intervals() {
        let process = network_process_fixture(1);
        let now = Instant::now();
        let mut store = HistoryStore::open(None);
        store.update(std::slice::from_ref(&process));
        let mut unavailable = network_interval(&process, 1_000_000, 2_000_000, 1.5, now);
        unavailable.available = false;
        unavailable.status = "ETW 权限不足".into();
        store.observe_network(&unavailable);
        let app = &store.app_history()[0];
        assert_eq!(app.network_received_bytes, None);
        assert_eq!(app.network_transmitted_bytes, None);
        assert_eq!(app.network_covered_seconds, None);
        assert!(store.network_note.as_ref().unwrap().contains("权限"));
        let valid = network_interval(&process, 13, 7, 1.5, now + Duration::from_secs(1));
        store.observe_network(&valid);
        store.observe_network(&valid.clone()); // Must not double count replay.
        store.observe_network(&unavailable); // Older snapshot cannot count either.
        let app = &store.app_history()[0];
        assert_eq!(app.network_received_bytes, Some(13));
        assert_eq!(app.network_transmitted_bytes, Some(7));
        assert_eq!(app.network_covered_seconds, Some(1.5));
        assert!(app.network_has_gaps);
        assert!(app.network_note.as_ref().unwrap().contains("进程秒"));
    }

    #[test]
    fn network_loss_pid_reuse_and_exit_preserve_only_observed_bytes() {
        let old = network_process_fixture(1);
        let mut new = old.clone();
        new.start_time_windows_ticks = old.start_time_windows_ticks.map(|ticks| ticks + 1);
        let now = Instant::now();
        let mut store = HistoryStore::open(None);
        store.update(std::slice::from_ref(&old));
        store.observe_network(&network_interval(&old, 11, 4, 1.0, now));
        let mut loss = network_interval(
            &old,
            9_000_000,
            8_000_000,
            1.0,
            now + Duration::from_secs(1),
        );
        loss.available = false;
        loss.complete_interval = false;
        loss.dropped_events = 1;
        loss.status = "丢失网络事件，本周期不可用".into();
        store.observe_network(&loss);
        assert_eq!(store.app_history()[0].network_received_bytes, Some(11));
        assert!(store.network_note.as_ref().unwrap().contains("丢失"));
        store.update(&[new.clone()]);
        // A buffered old identity must not contaminate its reused PID.
        store.observe_network(&network_interval(
            &old,
            10_000,
            10_000,
            1.0,
            now + Duration::from_secs(2),
        ));
        let new_entry = store
            .process_history()
            .into_iter()
            .find(|entry| entry.identity.start_time_windows_ticks == new.start_time_windows_ticks)
            .unwrap();
        assert_eq!(new_entry.network_received_bytes, None);
        let mut recovered = network_interval(&new, 3, 2, 0.5, now + Duration::from_secs(3));
        recovered.dropped_events = 1; // Lifetime loss count does not invalidate a later complete period.
        store.observe_network(&recovered);
        store.update(&[]);
        store.observe_network(&network_interval(
            &new,
            99_000,
            99_000,
            1.0,
            now + Duration::from_secs(4),
        ));
        let app = &store.app_history()[0];
        assert_eq!(app.network_received_bytes, Some(14));
        assert_eq!(app.network_transmitted_bytes, Some(6));
        assert_eq!(app.network_covered_seconds, Some(1.5));
        assert_eq!(app.process_instances, 2);
        assert_eq!(app.running_instances, 0);
        assert!(app.network_has_gaps);
    }

    #[test]
    fn network_first_or_identityless_intervals_remain_unavailable_and_idle_can_be_zero() {
        let process = network_process_fixture(1);
        let now = Instant::now();
        let mut store = HistoryStore::open(None);
        store.update(std::slice::from_ref(&process));
        let mut first = network_interval(&process, 10_000, 10_000, 1.0, now);
        first.complete_interval = false;
        first.status = "初始网络基线尚未就绪".into();
        store.observe_network(&first);
        assert_eq!(store.app_history()[0].network_received_bytes, None);
        store.observe_network(&network_interval(
            &process,
            0,
            0,
            1.0,
            now + Duration::from_secs(1),
        ));
        assert_eq!(store.app_history()[0].network_received_bytes, Some(0));
        assert_eq!(store.app_history()[0].network_covered_seconds, Some(1.0));
        let mut identityless = process.clone();
        identityless.start_time_windows_ticks = None;
        let mut store = HistoryStore::open(None);
        store.update(&[identityless]);
        store.observe_network(&network_interval(&process, 5_000, 5_000, 1.0, now));
        assert_eq!(store.app_history()[0].network_received_bytes, None);
        assert!(store.app_history()[0].network_has_gaps);
        let mut unverified = network_interval(&process, 5_000, 5_000, 1.0, now);
        unverified.interval_end = None;
        store.observe_network(&unverified);
        assert_eq!(store.app_history()[0].network_transmitted_bytes, None);
    }

    #[test]
    fn network_history_is_persistent_and_old_json_defaults_to_unavailable() {
        let path = history_fixture_path("network-history");
        let process = network_process_fixture(1);
        {
            let mut store = HistoryStore::open(Some(path.clone()));
            store.update(std::slice::from_ref(&process));
            store.observe_network(&network_interval(&process, 311, 23, 2.0, Instant::now()));
            store.persist().unwrap();
        }
        {
            let mut store = HistoryStore::open(Some(path.clone()));
            assert_eq!(store.app_history()[0].network_received_bytes, Some(311));
            store.update(std::slice::from_ref(&process));
            store.observe_network(&network_interval(&process, 7, 3, 0.75, Instant::now()));
            assert_eq!(store.app_history()[0].network_received_bytes, Some(318));
            assert_eq!(store.app_history()[0].network_covered_seconds, Some(2.75));
            store.persist().unwrap();
        }
        // Same v1 schema before network history existed: missing fields are accepted.
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for entry in legacy["entries"].as_array_mut().unwrap() {
            for key in [
                "network_received_bytes",
                "network_transmitted_bytes",
                "network_covered_seconds",
                "network_has_gaps",
            ] {
                entry.as_object_mut().unwrap().remove(key);
            }
        }
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        {
            let legacy = HistoryStore::open(Some(path.clone()));
            assert!(!legacy.write_blocked);
            assert_eq!(legacy.process_history().len(), 1);
            assert_eq!(legacy.app_history()[0].network_received_bytes, None);
            assert_eq!(legacy.app_history()[0].network_transmitted_bytes, None);
            assert_eq!(legacy.app_history()[0].network_covered_seconds, None);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn network_history_never_wraps_large_packet_totals_and_keeps_file_size_limit() {
        let mut first = network_process_fixture(1);
        first.pid = 101;
        let mut second = network_process_fixture(2);
        second.pid = 102;
        let now = Instant::now();
        let mut store = HistoryStore::open(None);
        store.update(&[first.clone(), second.clone()]);
        let mut interval = network_interval(&first, u64::MAX, 0, 1.0, now);
        interval
            .rates
            .extend(network_interval(&second, 1, 0, 1.0, now).rates);
        store.observe_network(&interval);
        assert_eq!(store.app_history()[0].network_received_bytes, None); // u128 sum exceeds UI u64.
        assert_eq!(store.app_history()[0].network_transmitted_bytes, Some(0));
        let mut next = network_interval(&first, 1, 0, 1.0, now + Duration::from_secs(1));
        next.rates.clear();
        next.rates
            .extend(network_interval(&first, 1, 0, 1.0, now).rates);
        store.observe_network(&next);
        let first_entry = store
            .process_history()
            .into_iter()
            .find(|entry| entry.identity.pid == 101)
            .unwrap();
        assert_eq!(first_entry.network_received_bytes, Some(u64::MAX));
        assert!(first_entry.network_has_gaps); // overflowing interval omitted, prior exact total retained.
        let path = history_fixture_path("network-limit");
        std::fs::write(&path, vec![b' '; 8 * 1024 * 1024 + 1]).unwrap();
        {
            let oversized = HistoryStore::open(Some(path.clone()));
            assert!(oversized.write_blocked);
            assert!(oversized.note.as_ref().unwrap().contains("8 MiB"));
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 8 * 1024 * 1024 + 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_snapshot_identity_and_counters_match_live_win32_queries() {
        use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
        use windows::Win32::System::Threading::{GetProcessIoCounters, IO_COUNTERS};
        let mut collector = ProcessPerformanceCollector::default();
        let samples = collector.sample().unwrap();
        let own = samples
            .iter()
            .find(|sample| sample.pid == std::process::id())
            .unwrap();
        let mut session = 0;
        unsafe { ProcessIdToSessionId(own.pid, &mut session) }.unwrap();
        assert_eq!(own.session_id, Some(session));
        assert!(own.thread_count.unwrap() > 0);
        assert!(own.handle_count.unwrap() > 0);
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
        .unwrap();
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        assert_eq!(own.start_time_windows_ticks, Some(ticks(created)));
        assert!(own.cpu_ticks.unwrap() <= ticks(kernel) + ticks(user));
        let mut io = IO_COUNTERS::default();
        unsafe { GetProcessIoCounters(GetCurrentProcess(), &mut io) }.unwrap();
        assert!(own.read_bytes.unwrap() <= io.ReadTransferCount);
        assert!(own.write_bytes.unwrap() <= io.WriteTransferCount);
        assert!(own.working_set_bytes.unwrap() > 0);
    }
    #[test]
    fn history_uses_counter_differences_and_separates_reused_pids() {
        let mut store = HistoryStore::open(None);
        let mut process = ProcessSample {
            pid: 123,
            start_time_unix_secs: 100,
            name: "app".into(),
            cpu_time_seconds: Some(10.0),
            total_disk_read_bytes: Some(100),
            total_disk_write_bytes: Some(50),
            ..Default::default()
        };
        store.update(std::slice::from_ref(&process));
        process.cpu_time_seconds = Some(12.5);
        process.total_disk_read_bytes = Some(160);
        process.total_disk_write_bytes = Some(70);
        store.update(std::slice::from_ref(&process));
        let app = &store.app_history()[0];
        assert_eq!(app.cpu_seconds, Some(2.5));
        assert_eq!(app.read_bytes, Some(60));
        assert_eq!(app.write_bytes, Some(20));
        process.start_time_unix_secs = 200;
        process.cpu_time_seconds = Some(1.0);
        process.total_disk_read_bytes = Some(5);
        store.update(&[process]);
        assert_eq!(store.process_history().len(), 2);
        assert_eq!(store.app_history()[0].cpu_seconds, Some(2.5));
        assert_eq!(store.app_history()[0].running_instances, 1);
    }

    #[test]
    fn history_does_not_fabricate_missing_counter_values() {
        let mut store = HistoryStore::open(None);
        store.update(&[ProcessSample {
            pid: 123,
            start_time_unix_secs: 100,
            name: "unknown".into(),
            ..Default::default()
        }]);
        assert_eq!(store.app_history()[0].cpu_seconds, None);
        assert_eq!(store.app_history()[0].read_bytes, None);
    }

    #[test]
    fn persisted_history_resumes_from_original_cumulative_baseline() {
        let path = std::env::temp_dir().join(format!(
            "systemmonitorx-history-test-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut process = ProcessSample {
            pid: 123,
            start_time_unix_secs: 100,
            start_time_windows_ticks: Some(116444737000000000),
            name: "persisted".into(),
            cpu_time_ticks: Some(100_000_000),
            total_disk_read_bytes: Some(100),
            ..Default::default()
        };
        {
            let mut store = HistoryStore::open(Some(path.clone()));
            store.update(std::slice::from_ref(&process));
            process.cpu_time_ticks = Some(125_000_000);
            process.total_disk_read_bytes = Some(160);
            store.update(std::slice::from_ref(&process));
            store.persist().unwrap();
        }
        {
            let mut store = HistoryStore::open(Some(path.clone()));
            assert_eq!(store.app_history()[0].cpu_seconds, Some(2.5));
            process.cpu_time_ticks = Some(130_000_000);
            process.total_disk_read_bytes = Some(190);
            store.update(&[process]);
            assert_eq!(store.app_history()[0].cpu_seconds, Some(3.0));
            assert_eq!(store.app_history()[0].read_bytes, Some(90));
            assert_eq!(store.process_history().len(), 1);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn history_is_bounded_and_preserves_subsecond_creation_identity() {
        let mut store = HistoryStore::open(None);
        let processes: Vec<_> = (1..=2050)
            .map(|pid| ProcessSample {
                pid,
                start_time_unix_secs: 100,
                name: "bounded".into(),
                ..Default::default()
            })
            .collect();
        store.update(&processes);
        assert_eq!(store.process_history().len(), 2048);
        let mut store = HistoryStore::open(None);
        let mut process = ProcessSample {
            pid: 123,
            start_time_unix_secs: 100,
            start_time_windows_ticks: Some(123000001),
            ..Default::default()
        };
        store.update(std::slice::from_ref(&process));
        process.start_time_windows_ticks = Some(123000002);
        store.update(&[process]);
        assert_eq!(store.process_history().len(), 2);
    }
}

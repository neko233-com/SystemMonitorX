//! Stateful system sampling. Construct and use `Monitor` on the same worker thread.
//!
//! All byte counts use bytes, all throughput uses bytes/second, and CPU percentages
//! use the whole machine as 100%. Missing counters remain `None`; an idle counter
//! that was successfully read is `Some(0.0)`. No simulated telemetry is used.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use sysinfo::{
    Components, CpuRefreshKind, DiskRefreshKind, Disks, Networks, ProcessRefreshKind,
    ProcessesToUpdate, System, UpdateKind,
};

use crate::gpu::{GpuMonitor, GpuSample};
use crate::network_process::ProcessNetworkMonitor;
use crate::performance::{
    AppHistorySample, HistoryStore, ProcessHistorySample, ProcessPerformanceCollector,
    system_performance,
};
use crate::process_groups::{
    ProcessGroup, WindowSnapshot, build_process_groups, enumerate_visible_windows,
};
use crate::sessions::{SessionSample, enumerate_sessions, summarize_sessions};

#[derive(Clone, Debug, Default)]
pub struct CpuSample {
    pub index: usize,
    pub name: String,
    pub usage_percent: f32,
    /// A reported zero frequency means the operating system did not provide it.
    pub frequency_mhz: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessSample {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub cpu_usage: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub working_set_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub private_working_set_bytes: Option<u64>,
    pub cpu_time_seconds: Option<f64>,
    pub cpu_time_ticks: Option<u64>,
    pub thread_count: Option<u32>,
    pub handle_count: Option<u32>,
    pub session_id: Option<u32>,
    pub user_name: Option<String>,
    pub disk_read_bytes_per_sec: Option<f64>,
    pub disk_write_bytes_per_sec: Option<f64>,
    pub total_disk_read_bytes: Option<u64>,
    pub total_disk_write_bytes: Option<u64>,
    pub gpu_usage: Option<f32>,
    pub gpu_memory_bytes: Option<u64>,
    /// Per-process network traffic requires a separate event tracing provider.
    pub network_received_bytes_per_sec: Option<f64>,
    pub network_transmitted_bytes_per_sec: Option<f64>,
    pub run_time_secs: u64,
    /// Together with `pid`, this identifies a process even when Windows reuses PIDs.
    pub start_time_unix_secs: u64,
    /// Exact native identity for mutations; avoids precision loss from Unix seconds.
    pub start_time_windows_ticks: Option<u64>,
    pub exe: Option<PathBuf>,
    pub status: String,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSample {
    /// Windows volume GUID; mount aliases for one volume share this identifier.
    pub id: String,
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    pub kind: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
    pub total_read_bytes: Option<u64>,
    pub total_write_bytes: Option<u64>,
    pub active_percent: Option<f32>,
    pub queue_depth: Option<u32>,
    pub read_latency_ms: Option<f64>,
    pub write_latency_ms: Option<f64>,
    pub removable: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, Default)]
pub struct NetworkSample {
    pub name: String,
    pub mac_address: String,
    pub state: String,
    pub received_bytes_per_sec: Option<f64>,
    pub transmitted_bytes_per_sec: Option<f64>,
    pub total_received_bytes: u64,
    pub total_transmitted_bytes: u64,
    pub ip_addresses: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TemperatureSample {
    pub label: String,
    pub celsius: Option<f32>,
    pub critical_celsius: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct SystemSnapshot {
    pub sampled_at: SystemTime,
    pub sample_interval_secs: f64,
    /// False until sysinfo has two CPU samples separated by its minimum interval.
    pub cpu_ready: bool,
    pub cpu_usage: f32,
    pub cpu_name: String,
    pub cpu_frequency_mhz: Option<u64>,
    pub logical_cpus: usize,
    pub physical_cores: Option<usize>,
    pub cpus: Vec<CpuSample>,
    pub memory_total: u64,
    pub memory_used: u64,
    pub memory_available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub processes: Vec<ProcessSample>,
    /// Indices refer to this exact processes vector; built on the collector worker.
    pub application_groups: Vec<ProcessGroup>,
    pub application_groups_note: Option<String>,
    pub disks: Vec<DiskSample>,
    pub networks: Vec<NetworkSample>,
    pub temperatures: Vec<TemperatureSample>,
    pub gpus: Vec<GpuSample>,
    pub network_receive_bytes_per_sec: Option<f64>,
    pub network_transmit_bytes_per_sec: Option<f64>,
    pub disk_read_bytes_per_sec: Option<f64>,
    pub disk_write_bytes_per_sec: Option<f64>,
    pub disk_usage: Option<f32>,
    pub uptime_secs: u64,
    pub boot_time_unix_secs: u64,
    pub host_name: String,
    pub os_name: String,
    pub os_version: String,
    pub kernel_version: String,
    pub architecture: String,
    pub temperature_note: Option<String>,
    pub gpu_note: Option<String>,
    pub io_note: Option<String>,
    pub network_note: Option<String>,
    pub process_network_available: bool,
    pub process_network_status: String,
    pub process_network_dropped_events: Option<u64>,
    pub system_handle_count: Option<u32>,
    pub system_thread_count: Option<u32>,
    pub system_process_count: Option<u32>,
    pub committed_bytes: Option<u64>,
    pub commit_limit_bytes: Option<u64>,
    pub commit_peak_bytes: Option<u64>,
    pub cached_memory_bytes: Option<u64>,
    pub kernel_paged_bytes: Option<u64>,
    pub kernel_nonpaged_bytes: Option<u64>,
    pub collector_duration_ms: f64,
    pub process_collection_note: Option<String>,
    pub sessions: Vec<SessionSample>,
    pub sessions_note: Option<String>,
    pub app_history: Vec<AppHistorySample>,
    pub history_note: Option<String>,
}

impl Default for SystemSnapshot {
    fn default() -> Self {
        Self {
            sampled_at: SystemTime::now(),
            sample_interval_secs: 0.0,
            cpu_ready: false,
            cpu_usage: 0.0,
            cpu_name: String::new(),
            cpu_frequency_mhz: None,
            logical_cpus: 0,
            physical_cores: None,
            cpus: Vec::new(),
            memory_total: 0,
            memory_used: 0,
            memory_available: 0,
            swap_total: 0,
            swap_used: 0,
            processes: Vec::new(),
            application_groups: Vec::new(),
            application_groups_note: None,
            disks: Vec::new(),
            networks: Vec::new(),
            temperatures: Vec::new(),
            gpus: Vec::new(),
            network_receive_bytes_per_sec: None,
            network_transmit_bytes_per_sec: None,
            disk_read_bytes_per_sec: None,
            disk_write_bytes_per_sec: None,
            disk_usage: None,
            uptime_secs: 0,
            boot_time_unix_secs: 0,
            host_name: String::new(),
            os_name: String::new(),
            os_version: String::new(),
            kernel_version: String::new(),
            architecture: String::new(),
            temperature_note: None,
            gpu_note: None,
            io_note: None,
            network_note: None,
            process_network_available: false,
            process_network_status: String::new(),
            process_network_dropped_events: None,
            system_handle_count: None,
            system_thread_count: None,
            system_process_count: None,
            committed_bytes: None,
            commit_limit_bytes: None,
            commit_peak_bytes: None,
            cached_memory_bytes: None,
            kernel_paged_bytes: None,
            kernel_nonpaged_bytes: None,
            collector_duration_ms: 0.0,
            process_collection_note: None,
            sessions: Vec::new(),
            sessions_note: None,
            app_history: Vec::new(),
            history_note: None,
        }
    }
}

impl SystemSnapshot {
    pub fn memory_percent(&self) -> f32 {
        percentage(self.memory_used, self.memory_total).unwrap_or(0.0)
    }

    pub fn swap_percent(&self) -> Option<f32> {
        percentage(self.swap_used, self.swap_total)
    }

    /// Task Manager convention: the busiest GPU engine, rather than their sum.
    pub fn gpu_percent(&self) -> Option<f32> {
        self.gpus
            .iter()
            .filter_map(|gpu| gpu.usage_percent)
            .filter(|value| value.is_finite())
            .max_by(f32::total_cmp)
    }
}

#[derive(Clone, Copy, Debug)]
struct IoCounters {
    read: u64,
    written: u64,
}

#[derive(Clone, Copy, Debug)]
struct IoBaseline {
    counters: IoCounters,
    sampled_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct CpuBaseline {
    ticks: u64,
    sampled_at: Instant,
}

#[derive(Clone, Debug)]
struct DiskCounters {
    id: String,
    io: IoCounters,
    idle_time: u64,
    query_time: u64,
    read_time: u64,
    write_time: u64,
    read_count: u32,
    write_count: u32,
    queue_depth: u32,
}

#[derive(Clone, Debug)]
struct DiskBaseline {
    counters: DiskCounters,
    sampled_at: Instant,
}

/// Long-lived collector. A one-second cadence gives useful CPU and rate samples.
/// `new()` primes counters; it does not sleep or manufacture a first utilization.
pub struct Monitor {
    system: System,
    disks: Disks,
    networks: Networks,
    components: Components,
    gpu: GpuMonitor,
    process_network: ProcessNetworkMonitor,
    process_collector: ProcessPerformanceCollector,
    process_baselines: HashMap<(u32, u64, u64), IoBaseline>,
    process_cpu_baselines: HashMap<(u32, u64, u64), CpuBaseline>,
    disk_baselines: HashMap<String, DiskBaseline>,
    disk_devices: HashMap<String, DiskDevice>,
    network_baselines: HashMap<(String, String), IoBaseline>,
    cpu_primed_at: Instant,
    slow_refresh_at: Instant,
    previous_snapshot_at: Instant,
    host_name: String,
    os_name: String,
    os_version: String,
    kernel_version: String,
    architecture: String,
    physical_cores: Option<usize>,
    sessions: Vec<SessionSample>,
    sessions_note: Option<String>,
    history: HistoryStore,
    app_history: Vec<AppHistorySample>,
    window_snapshot: WindowSnapshot,
    window_refreshed_at: Option<Instant>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    pub fn new() -> Self {
        Self::with_history(HistoryStore::open_default())
    }

    /// Diagnostic collectors keep benchmark/test processes out of saved user history.
    pub fn new_in_memory() -> Self {
        Self::with_history(HistoryStore::open(None))
    }

    fn with_history(history: HistoryStore) -> Self {
        let mut system = System::new();
        system.refresh_cpu_specifics(CpuRefreshKind::everything());
        let cpu_primed_at = Instant::now();
        system.refresh_memory();
        let mut process_collector = ProcessPerformanceCollector::default();
        let raw_processes = process_collector.sample().unwrap_or_default();
        let process_baselines = raw_processes
            .iter()
            .filter_map(|process| {
                let counters = IoCounters {
                    read: process.read_bytes?,
                    written: process.write_bytes?,
                };
                Some((
                    (
                        process.pid,
                        process.start_time_unix_secs,
                        process.start_time_windows_ticks.unwrap_or(0),
                    ),
                    IoBaseline {
                        counters,
                        sampled_at: process_collector.sampled_at(),
                    },
                ))
            })
            .collect();
        let process_cpu_baselines = raw_processes
            .iter()
            .filter_map(|process| {
                Some((
                    (
                        process.pid,
                        process.start_time_unix_secs,
                        process.start_time_windows_ticks.unwrap_or(0),
                    ),
                    CpuBaseline {
                        ticks: process.cpu_ticks?,
                        sampled_at: process_collector.sampled_at(),
                    },
                ))
            })
            .collect();

        // Native IO counters provide success/failure; sysinfo enumerates capacities.
        let disks = Disks::new_with_refreshed_list_specifics(
            DiskRefreshKind::nothing().with_storage().with_kind(),
        );
        let mut disk_devices = HashMap::new();
        let disk_baselines = disks
            .iter()
            .filter_map(|disk| {
                let counters = disk_io_counters(disk.mount_point(), &mut disk_devices)?;
                Some((
                    disk.mount_point().to_string_lossy().into_owned(),
                    DiskBaseline {
                        counters,
                        sampled_at: Instant::now(),
                    },
                ))
            })
            .collect();
        let networks = Networks::new_with_refreshed_list();
        let network_baselines = networks
            .iter()
            .map(|(name, network)| {
                (
                    (name.clone(), network.mac_address().to_string()),
                    IoBaseline {
                        counters: IoCounters {
                            read: network.total_received(),
                            written: network.total_transmitted(),
                        },
                        sampled_at: Instant::now(),
                    },
                )
            })
            .collect();
        let components = Components::new_with_refreshed_list();
        let (sessions, sessions_note) = match enumerate_sessions() {
            Ok(sessions) => (sessions, None),
            Err(error) => (Vec::new(), Some(format!("用户会话无法读取：{error:#}"))),
        };
        let now = Instant::now();

        Self {
            system,
            disks,
            networks,
            components,
            gpu: GpuMonitor::new(),
            process_network: ProcessNetworkMonitor::new(),
            process_collector,
            process_baselines,
            process_cpu_baselines,
            disk_baselines,
            disk_devices,
            network_baselines,
            cpu_primed_at,
            slow_refresh_at: now,
            previous_snapshot_at: now,
            host_name: System::host_name().unwrap_or_else(|| "未知主机".into()),
            os_name: System::name().unwrap_or_else(|| "Windows".into()),
            os_version: System::long_os_version()
                .or_else(System::os_version)
                .unwrap_or_else(|| "未知版本".into()),
            kernel_version: System::kernel_version().unwrap_or_else(|| "未知".into()),
            architecture: System::cpu_arch(),
            physical_cores: System::physical_core_count(),
            sessions,
            sessions_note,
            history,
            app_history: Vec::new(),
            window_snapshot: WindowSnapshot::default(),
            window_refreshed_at: None,
        }
    }

    pub fn sample(&mut self) -> SystemSnapshot {
        let collector_started = Instant::now();
        let cpu_ready = self.cpu_primed_at.elapsed() >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL;
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        let slow_refresh = self.slow_refresh_at.elapsed() >= Duration::from_secs(5);
        if slow_refresh {
            self.system.refresh_cpu_frequency();
            self.components.refresh(true);
            self.disks
                .refresh_specifics(true, DiskRefreshKind::nothing().with_storage().with_kind());
            // Re-resolve identities during hotplug scans, not every sample.
            self.disk_devices.clear();
            match enumerate_sessions() {
                Ok(sessions) => {
                    self.sessions = sessions;
                    self.sessions_note = None;
                }
                Err(error) => {
                    self.sessions_note = Some(format!("用户会话暂时无法刷新：{error:#}"));
                }
            }
            self.slow_refresh_at = Instant::now();
        }

        let cpus: Vec<_> = self
            .system
            .cpus()
            .iter()
            .enumerate()
            .map(|(index, cpu)| CpuSample {
                index,
                name: cpu.name().to_owned(),
                usage_percent: finite_percent(cpu.cpu_usage()),
                frequency_mhz: nonzero(cpu.frequency()),
            })
            .collect();
        let logical_cpus = cpus.len();
        let frequencies: Vec<_> = cpus.iter().filter_map(|cpu| cpu.frequency_mhz).collect();
        let cpu_frequency_mhz = if frequencies.is_empty() {
            None
        } else {
            Some(
                (frequencies
                    .iter()
                    .map(|value| u128::from(*value))
                    .sum::<u128>()
                    / frequencies.len() as u128) as u64,
            )
        };

        let gpus = self.gpu.sample();
        let gpu_usage = self.gpu.process_usage();
        let gpu_memory = self.gpu.process_memory();

        let process_result = self.process_collector.sample();
        let process_time = self.process_collector.sampled_at();
        let mut next_process_baselines = HashMap::new();
        let mut next_cpu_baselines = HashMap::new();
        let process_collection_note;
        let mut processes: Vec<_> = match process_result {
            Ok(raw_processes) => {
                process_collection_note = None;
                let unix_now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|time| time.as_secs())
                    .unwrap_or(0);
                raw_processes
                    .into_iter()
                    .map(|process| {
                        let key = (
                            process.pid,
                            process.start_time_unix_secs,
                            process.start_time_windows_ticks.unwrap_or(0),
                        );
                        let current_io = process
                            .read_bytes
                            .zip(process.write_bytes)
                            .map(|(read, written)| IoCounters { read, written });
                        let (read_rate, write_rate) = io_rates(
                            current_io,
                            self.process_baselines.get(&key).copied(),
                            process_time,
                        );
                        if let Some(counters) = current_io {
                            next_process_baselines.insert(
                                key,
                                IoBaseline {
                                    counters,
                                    sampled_at: process_time,
                                },
                            );
                        }
                        let usage = process
                            .cpu_ticks
                            .zip(self.process_cpu_baselines.get(&key))
                            .and_then(|(ticks, previous)| {
                                let elapsed = process_time
                                    .checked_duration_since(previous.sampled_at)?
                                    .as_secs_f64();
                                counter_rate(ticks, previous.ticks, elapsed).map(
                                    |ticks_per_second| {
                                        finite_percent(
                                            (ticks_per_second
                                                / 10_000_000.0
                                                / logical_cpus.max(1) as f64
                                                * 100.0)
                                                as f32,
                                        )
                                    },
                                )
                            });
                        if let Some(ticks) = process.cpu_ticks {
                            next_cpu_baselines.insert(
                                key,
                                CpuBaseline {
                                    ticks,
                                    sampled_at: process_time,
                                },
                            );
                        }
                        ProcessSample {
                            pid: process.pid,
                            parent_pid: process.parent_pid,
                            name: process.name,
                            // The idle pseudo-process is not application CPU consumption.
                            cpu_usage: if process.pid == 0 {
                                0.0
                            } else {
                                usage.unwrap_or(0.0)
                            },
                            memory_bytes: process.working_set_bytes.unwrap_or(0),
                            virtual_memory_bytes: process.virtual_bytes.unwrap_or(0),
                            working_set_bytes: process.working_set_bytes,
                            private_bytes: process.private_bytes,
                            private_working_set_bytes: process.private_working_set_bytes,
                            cpu_time_seconds: process
                                .cpu_ticks
                                .map(|ticks| ticks as f64 / 10_000_000.0),
                            cpu_time_ticks: process.cpu_ticks,
                            thread_count: process.thread_count,
                            handle_count: process.handle_count,
                            session_id: process.session_id,
                            user_name: process.user_name,
                            disk_read_bytes_per_sec: read_rate,
                            disk_write_bytes_per_sec: write_rate,
                            total_disk_read_bytes: process.read_bytes,
                            total_disk_write_bytes: process.write_bytes,
                            gpu_usage: gpu_usage
                                .get(&process.pid)
                                .copied()
                                .filter(|value| value.is_finite()),
                            gpu_memory_bytes: gpu_memory.get(&process.pid).copied(),
                            run_time_secs: if process.start_time_unix_secs > 0 {
                                unix_now.saturating_sub(process.start_time_unix_secs)
                            } else {
                                0
                            },
                            start_time_unix_secs: process.start_time_unix_secs,
                            start_time_windows_ticks: process.start_time_windows_ticks,
                            exe: process.exe,
                            status: if process.pid == 0 {
                                "系统空闲".into()
                            } else {
                                process.status.unwrap_or_else(|| "状态不可用".into())
                            },
                            ..Default::default()
                        }
                    })
                    .collect()
            }
            Err(error) => {
                // Native layouts are not a stable public Win32 contract. Preserve
                // supported sysinfo collection if a future Windows query rejects it.
                process_collection_note =
                    Some(format!("使用兼容进程采集，部分详情不可用：{error:#}"));
                self.system.refresh_processes_specifics(
                    ProcessesToUpdate::All,
                    true,
                    process_refresh_kind(),
                );
                self.system
                    .processes()
                    .iter()
                    .map(|(pid, process)| {
                        let pid = pid.as_u32();
                        let key = (pid, process.start_time(), 0);
                        let io = process_io_counters(pid);
                        let (read, written) =
                            io_rates(io, self.process_baselines.get(&key).copied(), process_time);
                        if let Some(counters) = io {
                            next_process_baselines.insert(
                                key,
                                IoBaseline {
                                    counters,
                                    sampled_at: process_time,
                                },
                            );
                        }
                        ProcessSample {
                            pid,
                            parent_pid: process.parent().map(|parent| parent.as_u32()),
                            name: process.name().to_string_lossy().into_owned(),
                            cpu_usage: normalize_process_cpu(process.cpu_usage(), logical_cpus),
                            memory_bytes: process.memory(),
                            virtual_memory_bytes: process.virtual_memory(),
                            disk_read_bytes_per_sec: read,
                            disk_write_bytes_per_sec: written,
                            total_disk_read_bytes: io.map(|io| io.read),
                            total_disk_write_bytes: io.map(|io| io.written),
                            gpu_usage: gpu_usage.get(&pid).copied(),
                            gpu_memory_bytes: gpu_memory.get(&pid).copied(),
                            run_time_secs: process.run_time(),
                            start_time_unix_secs: process.start_time(),
                            exe: process
                                .exe()
                                .filter(|path| !path.as_os_str().is_empty())
                                .map(PathBuf::from),
                            status: process.status().to_string(),
                            ..Default::default()
                        }
                    })
                    .collect()
            }
        };
        // Stable tie-breakers keep equal-usage processes from jumping every refresh.
        processes.sort_by(|left, right| {
            right
                .cpu_usage
                .total_cmp(&left.cpu_usage)
                .then_with(|| right.memory_bytes.cmp(&left.memory_bytes))
                .then_with(|| left.pid.cmp(&right.pid))
        });
        self.process_baselines = next_process_baselines;
        self.process_cpu_baselines = next_cpu_baselines;
        let process_identities: Vec<_> = processes
            .iter()
            .filter_map(|process| Some((process.pid, process.start_time_windows_ticks?)))
            .collect();
        let process_network = self.process_network.sample(&process_identities);
        for process in &mut processes {
            if let Some(rate) = process_network.rates.get(&process.pid).filter(|rate| {
                process.start_time_windows_ticks == Some(rate.process_start_time_windows_ticks)
            }) {
                process.network_received_bytes_per_sec = Some(rate.received_bytes_per_sec);
                process.network_transmitted_bytes_per_sec = Some(rate.sent_bytes_per_sec);
            }
        }
        // Window discovery is read-only and off the UI thread. Cached evidence
        // is joined to each fresh sample only through exact PID/FILETIME keys.
        if self
            .window_refreshed_at
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(2))
        {
            self.window_snapshot = enumerate_visible_windows(&processes);
            self.window_refreshed_at = Some(Instant::now());
        }
        let application_groups = build_process_groups(&processes, &self.window_snapshot);
        summarize_sessions(&mut self.sessions, &processes);
        self.history.update(&processes);
        self.history.observe_network(&process_network);
        if slow_refresh || self.app_history.is_empty() {
            self.app_history = self.history.app_history();
        }
        let mut next_disk_baselines = HashMap::new();
        let mut disks: Vec<_> = self
            .disks
            .iter()
            .map(|disk| {
                let mount_point = disk.mount_point().to_string_lossy().into_owned();
                let counters = disk_io_counters(disk.mount_point(), &mut self.disk_devices);
                let now = Instant::now();
                let previous = self.disk_baselines.get(&mount_point);
                // A drive letter can be reused for another volume after hotplug.
                let previous = previous.filter(|old| {
                    counters
                        .as_ref()
                        .is_some_and(|current| current.id == old.counters.id)
                });
                let previous_io = previous.map(|old| IoBaseline {
                    counters: old.counters.io,
                    sampled_at: old.sampled_at,
                });
                let (read_rate, write_rate) = io_rates(
                    counters.as_ref().map(|counters| counters.io),
                    previous_io,
                    now,
                );
                let active_percent = counters
                    .as_ref()
                    .zip(previous)
                    .and_then(|(current, old)| disk_active_percent(current, &old.counters));
                let read_latency_ms = counters.as_ref().zip(previous).and_then(|(current, old)| {
                    average_latency_ms(
                        current.read_time,
                        old.counters.read_time,
                        current.read_count,
                        old.counters.read_count,
                    )
                });
                let write_latency_ms =
                    counters.as_ref().zip(previous).and_then(|(current, old)| {
                        average_latency_ms(
                            current.write_time,
                            old.counters.write_time,
                            current.write_count,
                            old.counters.write_count,
                        )
                    });
                let sample = DiskSample {
                    id: counters
                        .as_ref()
                        .map(|value| value.id.clone())
                        .unwrap_or_else(|| mount_point.clone()),
                    name: disk.name().to_string_lossy().into_owned(),
                    mount_point: mount_point.clone(),
                    file_system: disk.file_system().to_string_lossy().into_owned(),
                    kind: match disk.kind() {
                        sysinfo::DiskKind::SSD => "SSD".into(),
                        sysinfo::DiskKind::HDD => "HDD".into(),
                        sysinfo::DiskKind::Unknown(_) => "未知".into(),
                    },
                    total_bytes: disk.total_space(),
                    available_bytes: disk.available_space(),
                    read_bytes_per_sec: read_rate,
                    write_bytes_per_sec: write_rate,
                    total_read_bytes: counters.as_ref().map(|value| value.io.read),
                    total_write_bytes: counters.as_ref().map(|value| value.io.written),
                    active_percent,
                    queue_depth: counters.as_ref().map(|value| value.queue_depth),
                    read_latency_ms,
                    write_latency_ms,
                    removable: disk.is_removable(),
                    read_only: disk.is_read_only(),
                };
                if let Some(counters) = counters {
                    next_disk_baselines.insert(
                        mount_point,
                        DiskBaseline {
                            counters,
                            sampled_at: now,
                        },
                    );
                }
                sample
            })
            .collect();
        disks.sort_by(|left, right| left.mount_point.cmp(&right.mount_point));
        self.disk_baselines = next_disk_baselines;

        self.networks.refresh(true);
        let mut next_network_baselines = HashMap::new();
        let mut networks: Vec<_> = self
            .networks
            .iter()
            .map(|(name, network)| {
                let mac_address = network.mac_address().to_string();
                let key = (name.clone(), mac_address.clone());
                let counters = IoCounters {
                    read: network.total_received(),
                    written: network.total_transmitted(),
                };
                let now = Instant::now();
                let (received_rate, transmitted_rate) = io_rates(
                    Some(counters),
                    self.network_baselines.get(&key).copied(),
                    now,
                );
                next_network_baselines.insert(
                    key,
                    IoBaseline {
                        counters,
                        sampled_at: now,
                    },
                );
                NetworkSample {
                    name: name.clone(),
                    mac_address,
                    state: network.operational_state().to_string(),
                    received_bytes_per_sec: received_rate,
                    transmitted_bytes_per_sec: transmitted_rate,
                    total_received_bytes: counters.read,
                    total_transmitted_bytes: counters.written,
                    ip_addresses: network
                        .ip_networks()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                }
            })
            .collect();
        networks.sort_by(|left, right| left.name.cmp(&right.name));
        self.network_baselines = next_network_baselines;

        let temperatures: Vec<_> = self
            .components
            .iter()
            .map(|component| TemperatureSample {
                label: component.label().to_owned(),
                celsius: component.temperature().filter(|value| value.is_finite()),
                critical_celsius: component.critical().filter(|value| value.is_finite()),
            })
            .collect();
        let temperature_note = if temperatures.iter().all(|value| value.celsius.is_none()) {
            Some("此设备未向 Windows 提供可读取的温度传感器。GPU 温度取决于显卡驱动支持。".into())
        } else {
            Some("系统温度来自 Windows 热区传感器，不一定对应 CPU 核心温度。".into())
        };
        let gpu_note = self
            .gpu
            .process_memory_note()
            .map(str::to_owned)
            .or_else(|| gpus.is_empty().then(|| "未检测到可用的图形适配器。".into()));
        let io_note = disks
            .iter()
            .any(|disk| disk.total_read_bytes.is_none())
            .then(|| "部分卷未提供 I/O 计数；相关速率显示为不可用。".into());
        let disk_read_bytes_per_sec = unique_disk_rate(&disks, |disk| disk.read_bytes_per_sec);
        let disk_write_bytes_per_sec = unique_disk_rate(&disks, |disk| disk.write_bytes_per_sec);
        let disk_usage = disks
            .iter()
            .filter_map(|disk| disk.active_percent)
            .max_by(f32::total_cmp);
        let network_receive_bytes_per_sec = sum_known_rates(
            networks
                .iter()
                .map(|network| network.received_bytes_per_sec),
        );
        let network_transmit_bytes_per_sec = sum_known_rates(
            networks
                .iter()
                .map(|network| network.transmitted_bytes_per_sec),
        );
        let now = Instant::now();
        let sample_interval_secs = now.duration_since(self.previous_snapshot_at).as_secs_f64();
        self.previous_snapshot_at = now;
        let performance = system_performance();

        SystemSnapshot {
            sampled_at: SystemTime::now(),
            sample_interval_secs,
            cpu_ready,
            cpu_usage: finite_percent(self.system.global_cpu_usage()),
            cpu_name: self
                .system
                .cpus()
                .first()
                .map(|cpu| cpu.brand().to_owned())
                .unwrap_or_else(|| "未知 CPU".into()),
            cpu_frequency_mhz,
            logical_cpus,
            physical_cores: self.physical_cores,
            cpus,
            memory_total: self.system.total_memory(),
            memory_used: self.system.used_memory(),
            memory_available: self.system.available_memory(),
            swap_total: self.system.total_swap(),
            swap_used: self.system.used_swap(),
            processes,
            application_groups,
            application_groups_note: self.window_snapshot.note.clone(),
            disks,
            networks,
            temperatures,
            gpus,
            network_receive_bytes_per_sec,
            network_transmit_bytes_per_sec,
            disk_read_bytes_per_sec,
            disk_write_bytes_per_sec,
            disk_usage,
            uptime_secs: System::uptime(),
            boot_time_unix_secs: System::boot_time(),
            host_name: self.host_name.clone(),
            os_name: self.os_name.clone(),
            os_version: self.os_version.clone(),
            kernel_version: self.kernel_version.clone(),
            architecture: self.architecture.clone(),
            temperature_note,
            gpu_note,
            io_note,
            network_note: Some(format!(
                "显示 Windows 提供的活动网络适配器；{}",
                process_network.status
            )),
            process_network_available: process_network.available,
            process_network_dropped_events: if process_network.available
                || process_network.dropped_events > 0
            {
                Some(process_network.dropped_events)
            } else {
                None
            },
            process_network_status: process_network.status,
            system_handle_count: performance.handle_count,
            system_thread_count: performance.thread_count,
            system_process_count: performance.process_count,
            committed_bytes: performance.committed_bytes,
            commit_limit_bytes: performance.commit_limit_bytes,
            commit_peak_bytes: performance.commit_peak_bytes,
            cached_memory_bytes: performance.cached_memory_bytes,
            kernel_paged_bytes: performance.kernel_paged_bytes,
            kernel_nonpaged_bytes: performance.kernel_nonpaged_bytes,
            collector_duration_ms: collector_started.elapsed().as_secs_f64() * 1000.0,
            process_collection_note,
            sessions: self.sessions.clone(),
            sessions_note: self.sessions_note.clone(),
            app_history: self.app_history.clone(),
            history_note: Some(format!(
                "仅统计已观察进程；不含采样间隔内启动并退出的进程。最多保留 2048 个进程实例。{} {}",
                self.history.note.as_deref().unwrap_or_default(),
                self.history
                    .network_note
                    .as_deref()
                    .unwrap_or("网络历史尚无完整有效的 ETW 采样周期。"),
            )),
        }
    }

    pub fn process_history(&self) -> Vec<ProcessHistorySample> {
        self.history.process_history()
    }

    /// Call only in response to an explicit reset action on the collector thread.
    pub fn reset_history(&mut self) -> anyhow::Result<()> {
        self.history.reset()?;
        self.app_history.clear();
        Ok(())
    }
}

fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .without_tasks()
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn finite_percent(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn normalize_process_cpu(raw_usage: f32, logical_cpus: usize) -> f32 {
    if logical_cpus == 0 {
        0.0
    } else {
        finite_percent(raw_usage / logical_cpus as f32)
    }
}

fn percentage(used: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| finite_percent((used as f64 / total as f64 * 100.0) as f32))
}

/// Counter resets and missing baselines are unavailable, not huge rates or idle data.
fn counter_rate(current: u64, previous: u64, elapsed_secs: f64) -> Option<f64> {
    if !elapsed_secs.is_finite() || elapsed_secs <= 0.0 {
        return None;
    }
    let difference = current.checked_sub(previous)?;
    let rate = difference as f64 / elapsed_secs;
    rate.is_finite().then_some(rate)
}

fn io_rates(
    current: Option<IoCounters>,
    previous: Option<IoBaseline>,
    now: Instant,
) -> (Option<f64>, Option<f64>) {
    let (Some(current), Some(previous)) = (current, previous) else {
        return (None, None);
    };
    let Some(elapsed) = now.checked_duration_since(previous.sampled_at) else {
        return (None, None);
    };
    let elapsed_secs = elapsed.as_secs_f64();
    (
        counter_rate(current.read, previous.counters.read, elapsed_secs),
        counter_rate(current.written, previous.counters.written, elapsed_secs),
    )
}

fn disk_active_percent(current: &DiskCounters, previous: &DiskCounters) -> Option<f32> {
    let elapsed = current.query_time.checked_sub(previous.query_time)?;
    let idle = current.idle_time.checked_sub(previous.idle_time)?;
    if elapsed == 0 || idle > elapsed {
        return None;
    }
    Some(finite_percent(
        (1.0 - idle as f64 / elapsed as f64) as f32 * 100.0,
    ))
}

fn average_latency_ms(
    current_time: u64,
    previous_time: u64,
    current_count: u32,
    previous_count: u32,
) -> Option<f64> {
    let count = current_count.checked_sub(previous_count)?;
    if count == 0 {
        return None;
    }
    let time = current_time.checked_sub(previous_time)?;
    Some(time as f64 / 10_000.0 / f64::from(count))
}

fn sum_known_rates(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut sum = 0.0;
    let mut any = false;
    for value in values.flatten().filter(|value| value.is_finite()) {
        any = true;
        sum += value;
    }
    any.then_some(sum)
}

fn unique_disk_rate(
    disks: &[DiskSample],
    select: impl Fn(&DiskSample) -> Option<f64>,
) -> Option<f64> {
    let mut seen = HashSet::new();
    sum_known_rates(
        disks
            .iter()
            .filter(|disk| seen.insert(disk.id.as_str()))
            .map(select),
    )
}

#[cfg(windows)]
fn process_io_counters(pid: u32) -> Option<IoCounters> {
    use windows::Win32::System::Threading::{
        GetProcessIoCounters, IO_COUNTERS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: the PID comes from a fresh process enumeration. The returned handle
    // is owned and closed by NativeHandle; the output buffer has the required size.
    let handle =
        NativeHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut counters = IO_COUNTERS::default();
    unsafe { GetProcessIoCounters(handle.0, &mut counters) }.ok()?;
    Some(IoCounters {
        read: counters.ReadTransferCount,
        written: counters.WriteTransferCount,
    })
}

#[cfg(windows)]
struct NativeHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for NativeHandle {
    fn drop(&mut self) {
        // SAFETY: NativeHandle is only constructed from successful owned handles.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
struct DiskDevice {
    id: String,
    handle: NativeHandle,
}

#[cfg(not(windows))]
struct DiskDevice;

#[cfg(windows)]
fn disk_io_counters(
    mount_point: &std::path::Path,
    devices: &mut HashMap<String, DiskDevice>,
) -> Option<DiskCounters> {
    let key = mount_point.to_string_lossy().into_owned();
    if !devices.contains_key(&key) {
        devices.insert(key.clone(), open_disk_device(mount_point)?);
    }
    let result = read_disk_device(devices.get(&key)?);
    if result.is_none() {
        // A removed device must be reopened and verified on the next sample.
        devices.remove(&key);
    }
    result
}

#[cfg(windows)]
fn open_disk_device(mount_point: &std::path::Path) -> Option<DiskDevice> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetVolumeNameForVolumeMountPointW, OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    let mount: Vec<u16> = mount_point.as_os_str().encode_wide().chain([0]).collect();
    let mut volume = [0u16; 512];
    // SAFETY: the input is null terminated and output is a valid bounded buffer.
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount.as_ptr()), &mut volume) }.ok()?;
    let end = volume.iter().position(|value| *value == 0)?;
    let id = String::from_utf16_lossy(&volume[..end]);
    // Device handles require the volume GUID path without its trailing separator.
    if end == 0 || volume[end - 1] != b'\\' as u16 {
        return None;
    }
    volume[end - 1] = 0;
    // SAFETY: the volume path is null terminated, no write access is requested,
    // and all other parameters follow the documented synchronous open contract.
    let handle = NativeHandle(
        unsafe {
            CreateFileW(
                PCWSTR(volume.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        }
        .ok()?,
    );
    Some(DiskDevice { id, handle })
}

#[cfg(windows)]
fn read_disk_device(device: &DiskDevice) -> Option<DiskCounters> {
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};
    let mut counters = DISK_PERFORMANCE::default();
    let mut returned = 0;
    // SAFETY: this synchronous IOCTL accepts no input and the output buffer is a
    // properly aligned DISK_PERFORMANCE with its exact byte size.
    unsafe {
        DeviceIoControl(
            device.handle.0,
            IOCTL_DISK_PERFORMANCE,
            None,
            0,
            Some((&mut counters as *mut DISK_PERFORMANCE).cast()),
            std::mem::size_of::<DISK_PERFORMANCE>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .ok()?;
    if returned < std::mem::size_of::<DISK_PERFORMANCE>() as u32 {
        return None;
    }
    Some(DiskCounters {
        id: device.id.clone(),
        io: IoCounters {
            read: counters.BytesRead.try_into().ok()?,
            written: counters.BytesWritten.try_into().ok()?,
        },
        idle_time: counters.IdleTime.try_into().ok()?,
        query_time: counters.QueryTime.try_into().ok()?,
        read_time: counters.ReadTime.try_into().ok()?,
        write_time: counters.WriteTime.try_into().ok()?,
        read_count: counters.ReadCount,
        write_count: counters.WriteCount,
        queue_depth: counters.QueueDepth,
    })
}

#[cfg(not(windows))]
fn process_io_counters(_pid: u32) -> Option<IoCounters> {
    None
}

#[cfg(not(windows))]
fn disk_io_counters(
    _mount_point: &std::path::Path,
    _devices: &mut HashMap<String, DiskDevice>,
) -> Option<DiskCounters> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_uses_the_whole_machine_as_one_hundred_percent() {
        assert_eq!(normalize_process_cpu(100.0, 8), 12.5);
        assert_eq!(normalize_process_cpu(800.0, 8), 100.0);
        assert_eq!(normalize_process_cpu(2_000.0, 8), 100.0);
        assert_eq!(normalize_process_cpu(f32::NAN, 8), 0.0);
        assert_eq!(normalize_process_cpu(-1.0, 8), 0.0);
        assert_eq!(normalize_process_cpu(100.0, 0), 0.0);
    }

    #[test]
    fn rates_follow_actual_elapsed_time_and_do_not_spike_after_counter_reset() {
        assert_eq!(counter_rate(7_000, 1_000, 1.5), Some(4_000.0));
        assert_eq!(counter_rate(5, 5, 1.0), Some(0.0));
        assert_eq!(counter_rate(5, 10, 1.0), None);
        assert_eq!(counter_rate(10, 5, 0.0), None);
        assert_eq!(counter_rate(10, 5, f64::NAN), None);
    }

    #[test]
    fn first_and_failed_io_reads_are_unavailable() {
        let now = Instant::now();
        let zero = IoCounters {
            read: 0,
            written: 0,
        };
        assert_eq!(io_rates(Some(zero), None, now), (None, None));
        assert_eq!(
            io_rates(
                None,
                Some(IoBaseline {
                    counters: zero,
                    sampled_at: now
                }),
                now
            ),
            (None, None)
        );
        let old = IoBaseline {
            counters: zero,
            sampled_at: now - Duration::from_secs(1),
        };
        assert_eq!(io_rates(Some(zero), Some(old), now), (Some(0.0), Some(0.0)));
    }

    #[test]
    fn multiple_mounts_of_one_volume_are_not_counted_twice() {
        let disks = vec![
            DiskSample {
                id: "volume-a".into(),
                read_bytes_per_sec: Some(100.0),
                ..Default::default()
            },
            DiskSample {
                id: "volume-a".into(),
                read_bytes_per_sec: Some(100.0),
                ..Default::default()
            },
            DiskSample {
                id: "volume-b".into(),
                read_bytes_per_sec: Some(50.0),
                ..Default::default()
            },
        ];
        assert_eq!(
            unique_disk_rate(&disks, |disk| disk.read_bytes_per_sec),
            Some(150.0)
        );
        assert_eq!(sum_known_rates([None, None].into_iter()), None);
        assert_eq!(average_latency_ms(200_000, 100_000, 12, 10), Some(5.0));
        assert_eq!(average_latency_ms(200_000, 100_000, 10, 10), None);
    }
}

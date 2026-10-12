//! Snapshot-derived summaries, not a health or bottleneck diagnosis.
//! No I/O; source order remains unchanged; rankings retain three consumers.
//! CPU percentages already use the whole machine as 100%; do not divide again.
use crate::monitor::{ProcessSample, SystemSnapshot};
use crate::name_order::compare_names;
use crate::process_groups::ProcessIdentity;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub const TOP_CONSUMER_LIMIT: usize = 3;
pub const DISK_CAPACITY_HINT_LIMIT: usize = 3;
pub const GIB: u64 = 1024 * 1024 * 1024;
const MIN_STALE_AFTER_MILLIS: u64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpAction {
    Processes,
    Performance,
    Startup,
    Ports,
    Devices,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Cpu,
    Memory,
    Disk,
    Gpu,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedLoadLevel {
    BelowHigh,
    High,
    VeryHigh,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricAvailability {
    Available,
    WarmingUp,
    NoDevice,
    MissingCounter,
    InvalidCounter,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnit {
    Percent,
    Bytes,
    BytesPerSecond,
}
/// Byte evidence retains exact integers, including in JSON exports.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricValue {
    Percent(f64),
    Bytes(u64),
    BytesPerSecond(f64),
}
impl MetricValue {
    pub fn unit(self) -> MetricUnit {
        match self {
            Self::Percent(_) => MetricUnit::Percent,
            Self::Bytes(_) => MetricUnit::Bytes,
            Self::BytesPerSecond(_) => MetricUnit::BytesPerSecond,
        }
    }
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Percent(value) | Self::BytesPerSecond(value) => value,
            Self::Bytes(value) => value as f64,
        }
    }
    fn compare(self, other: Self) -> Ordering {
        match (self, other) {
            (Self::Bytes(left), Self::Bytes(right)) => left.cmp(&right),
            _ => self.as_f64().total_cmp(&other.as_f64()),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricSource {
    GlobalCpu,
    SystemMemory,
    VolumeActiveTime,
    GpuEngineUtilization,
    ProcessCpu,
    ProcessWorkingSet,
    ProcessMemoryBytes,
    ProcessIoRates,
    ProcessGpuEngineUtilization,
    VolumeCapacity,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricScope {
    WholeMachineCpu,
    PhysicalMemory,
    BusiestMeasuredVolume,
    BusiestMeasuredGpuEngine,
    ProcessWholeMachineCpu,
    ProcessWorkingSet,
    ProcessReadWriteIo,
    ProcessBusiestGpuEngine,
    Volume,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MetricEvidence {
    pub value: MetricValue,
    pub source: MetricSource,
    pub scope: MetricScope,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LoadThresholds {
    /// Inclusive percentage thresholds, not health recommendations.
    pub high_percent: f64,
    pub very_high_percent: f64,
}
impl LoadThresholds {
    pub const DEFAULT: Self = Self {
        high_percent: 80.0,
        very_high_percent: 95.0,
    };
    pub const MEMORY: Self = Self {
        high_percent: 85.0,
        very_high_percent: 95.0,
    };
    fn classify(self, value: f64) -> ObservedLoadLevel {
        if value >= self.very_high_percent {
            ObservedLoadLevel::VeryHigh
        } else if value >= self.high_percent {
            ObservedLoadLevel::High
        } else {
            ObservedLoadLevel::BelowHigh
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CounterCoverage {
    pub measured: usize,
    pub enumerated: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ObservedLoad {
    pub resource: ResourceKind,
    pub level: ObservedLoadLevel,
    pub availability: MetricAvailability,
    pub measurement: Option<MetricEvidence>,
    pub thresholds: LoadThresholds,
    pub coverage: CounterCoverage,
    pub help: HelpAction,
}
impl ObservedLoad {
    fn new(
        resource: ResourceKind,
        value: Option<f64>,
        unavailable: MetricAvailability,
        source: MetricSource,
        scope: MetricScope,
        coverage: CounterCoverage,
    ) -> Self {
        let thresholds = if resource == ResourceKind::Memory {
            LoadThresholds::MEMORY
        } else {
            LoadThresholds::DEFAULT
        };
        Self {
            resource,
            level: value.map_or(ObservedLoadLevel::Unavailable, |value| {
                thresholds.classify(value)
            }),
            availability: if value.is_some() {
                MetricAvailability::Available
            } else {
                unavailable
            },
            measurement: value.map(|value| MetricEvidence {
                value: MetricValue::Percent(value),
                source,
                scope,
            }),
            thresholds,
            coverage,
            help: HelpAction::Performance,
        }
    }
    pub fn percent(&self) -> Option<f64> {
        match self.measurement?.value {
            MetricValue::Percent(value) => Some(value),
            _ => None,
        }
    }
}
/// The UI supplies monotonic receipt age; this module reads no clocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SnapshotObservedAge {
    pub age_millis: Option<u64>,
    pub paused: bool,
}
impl SnapshotObservedAge {
    pub fn new(age: Duration, paused: bool) -> Self {
        Self {
            age_millis: Some(u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
            paused,
        }
    }
}
/// Current means within the displayed age allowance, never overall health.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFreshness {
    Current,
    Paused,
    Delayed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TopConsumer {
    pub pid: u32,
    pub creation_time_windows_ticks: Option<u64>,
    pub start_time_unix_secs: u64,
    /// Original raw process name.
    pub name: String,
    pub measurement: MetricEvidence,
    pub help: HelpAction,
}
impl TopConsumer {
    /// Navigation identity only. Existing native mutation guards remain required.
    pub fn exact_identity(&self) -> Option<ProcessIdentity> {
        Some(ProcessIdentity {
            pid: self.pid,
            creation_time_windows_ticks: self
                .creation_time_windows_ticks
                .filter(|ticks| *ticks > 0 && self.pid > 0)?,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskSpaceLevel {
    Low,
    VeryLow,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiskCapacityHint {
    pub id: String,
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub available_percent: f64,
    pub level: DiskSpaceLevel,
    /// Both inclusive bounds must hold; large absolute free space is not low.
    pub threshold_percent: f64,
    pub threshold_bytes: u64,
    pub source: MetricSource,
    pub read_only: bool,
    pub removable: bool,
    pub help: HelpAction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryCapacityEvidence {
    pub total_bytes: u64,
    pub used_bytes: u64,
    /// Independently reported, not inferred as total minus used.
    pub available_bytes: Option<u64>,
    pub source: MetricSource,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResourceOverview {
    pub observed_age: SnapshotObservedAge,
    pub freshness: SnapshotFreshness,
    pub stale_after_millis: u64,
    pub cpu: ObservedLoad,
    pub memory: ObservedLoad,
    pub disk: ObservedLoad,
    pub gpu: ObservedLoad,
    pub memory_capacity: Option<MemoryCapacityEvidence>,
    pub process_count: usize,
    pub exact_identity_process_count: usize,
    pub top_cpu: Vec<TopConsumer>,
    pub top_memory: Vec<TopConsumer>,
    pub top_disk_io: Vec<TopConsumer>,
    pub top_gpu: Vec<TopConsumer>,
    pub low_disk_space: Vec<DiskCapacityHint>,
    pub disk_capacity_coverage: CounterCoverage,
}
impl ResourceOverview {
    /// Update UI receipt age/pause in O(1) without rebuilding resource rankings.
    pub fn set_observed_age(&mut self, observed_age: SnapshotObservedAge) {
        self.observed_age = observed_age;
        self.freshness = if observed_age.paused {
            SnapshotFreshness::Paused
        } else {
            match observed_age.age_millis {
                Some(age) if age > self.stale_after_millis => SnapshotFreshness::Delayed,
                Some(_) => SnapshotFreshness::Current,
                None => SnapshotFreshness::Unknown,
            }
        };
    }
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum VolumeKey<'a> {
    Id(&'a str),
    Mount(&'a str),
    Unidentified(usize),
}
#[derive(Clone, Copy)]
struct Candidate<'a> {
    process: &'a ProcessSample,
    measurement: MetricEvidence,
}
fn compare_candidates(left: &Candidate<'_>, right: &Candidate<'_>) -> Ordering {
    right
        .measurement
        .value
        .compare(left.measurement.value)
        .then_with(|| compare_names(&left.process.name, &right.process.name))
        .then(left.process.pid.cmp(&right.process.pid))
        .then(
            left.process
                .start_time_windows_ticks
                .filter(|ticks| *ticks > 0)
                .cmp(
                    &right
                        .process
                        .start_time_windows_ticks
                        .filter(|ticks| *ticks > 0),
                ),
        )
        .then(
            left.process
                .start_time_unix_secs
                .cmp(&right.process.start_time_unix_secs),
        )
        .then(left.process.name.cmp(&right.process.name))
}
fn insert_consumer<'a>(list: &mut Vec<Candidate<'a>>, candidate: Candidate<'a>) {
    let position = list
        .iter()
        .position(|current| compare_candidates(&candidate, current) == Ordering::Less)
        .unwrap_or(list.len());
    if position < TOP_CONSUMER_LIMIT {
        list.insert(position, candidate);
        list.truncate(TOP_CONSUMER_LIMIT);
    }
}
fn finish_consumers(candidates: Vec<Candidate<'_>>) -> Vec<TopConsumer> {
    candidates
        .into_iter()
        .map(|candidate| TopConsumer {
            pid: candidate.process.pid,
            creation_time_windows_ticks: candidate
                .process
                .start_time_windows_ticks
                .filter(|ticks| *ticks > 0),
            start_time_unix_secs: candidate.process.start_time_unix_secs,
            name: candidate.process.name.clone(),
            measurement: candidate.measurement,
            help: HelpAction::Processes,
        })
        .collect()
}
fn percent(value: f32) -> Option<f64> {
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(f64::from(value))
}
fn rate(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}
fn stale_after_millis(interval_secs: f64) -> u64 {
    if interval_secs.is_finite() && interval_secs > 0.0 {
        (interval_secs.mul_add(3_000.0, 0.0).ceil() as u64).max(MIN_STALE_AFTER_MILLIS)
    } else {
        MIN_STALE_AFTER_MILLIS
    }
}
fn compare_capacity(left: &DiskCapacityHint, right: &DiskCapacityHint) -> Ordering {
    left.available_percent
        .total_cmp(&right.available_percent)
        .then(left.available_bytes.cmp(&right.available_bytes))
        .then_with(|| compare_names(&left.mount_point, &right.mount_point))
        .then_with(|| compare_names(&left.name, &right.name))
        .then(left.id.cmp(&right.id))
        .then(left.mount_point.cmp(&right.mount_point))
        .then(left.name.cmp(&right.name))
}
fn insert_capacity(list: &mut Vec<DiskCapacityHint>, hint: DiskCapacityHint) {
    let position = list
        .iter()
        .position(|current| compare_capacity(&hint, current) == Ordering::Less)
        .unwrap_or(list.len());
    if position < DISK_CAPACITY_HINT_LIMIT {
        list.insert(position, hint);
        list.truncate(DISK_CAPACITY_HINT_LIMIT);
    }
}
/// Cache with the exact accepted snapshot. O(processes + volumes + GPUs);
/// only fixed-size three-entry lists are sorted, never the main A-Z table.
pub fn build_overview(
    snapshot: &SystemSnapshot,
    observed_age: SnapshotObservedAge,
) -> ResourceOverview {
    let stale_after_millis = stale_after_millis(snapshot.sample_interval_secs);
    let freshness = if observed_age.paused {
        SnapshotFreshness::Paused
    } else {
        match observed_age.age_millis {
            Some(age) if age > stale_after_millis => SnapshotFreshness::Delayed,
            Some(_) => SnapshotFreshness::Current,
            None => SnapshotFreshness::Unknown,
        }
    };
    let cpu_value = snapshot
        .cpu_ready
        .then(|| percent(snapshot.cpu_usage))
        .flatten();
    let cpu_unavailable = if !snapshot.cpu_ready {
        MetricAvailability::WarmingUp
    } else {
        MetricAvailability::InvalidCounter
    };
    let memory_capacity = (snapshot.memory_total > 0
        && snapshot.memory_used <= snapshot.memory_total)
        .then_some(MemoryCapacityEvidence {
            total_bytes: snapshot.memory_total,
            used_bytes: snapshot.memory_used,
            available_bytes: (snapshot.memory_available <= snapshot.memory_total)
                .then_some(snapshot.memory_available),
            source: MetricSource::SystemMemory,
        });
    let memory_value =
        memory_capacity.map(|memory| memory.used_bytes as f64 / memory.total_bytes as f64 * 100.0);
    let mut disk_sources = HashMap::new();
    let mut capacity_sources = HashSet::new();
    let mut disk_value: Option<f64> = None;
    let mut low_disk_space = Vec::with_capacity(DISK_CAPACITY_HINT_LIMIT);
    let mut invalid_disk_counter = false;
    for (index, disk) in snapshot.disks.iter().enumerate() {
        let key = if !disk.id.is_empty() {
            VolumeKey::Id(&disk.id)
        } else if !disk.mount_point.is_empty() {
            VolumeKey::Mount(&disk.mount_point)
        } else {
            VolumeKey::Unidentified(index)
        };
        let active = disk.active_percent.and_then(percent);
        invalid_disk_counter |= disk.active_percent.is_some() && active.is_none();
        let measured = disk_sources.entry(key).or_insert(false);
        *measured |= active.is_some();
        if let Some(active) = active {
            disk_value = Some(disk_value.map_or(active, |old| old.max(active)));
        }
        if disk.total_bytes == 0
            || disk.available_bytes > disk.total_bytes
            || !capacity_sources.insert(key)
        {
            continue;
        }
        let available_percent = disk.available_bytes as f64 / disk.total_bytes as f64 * 100.0;
        let (level, threshold_percent, threshold_bytes) = if u128::from(disk.available_bytes) * 100
            <= u128::from(disk.total_bytes) * 5
            && disk.available_bytes <= 2 * GIB
        {
            (DiskSpaceLevel::VeryLow, 5.0, 2 * GIB)
        } else if u128::from(disk.available_bytes) * 100 <= u128::from(disk.total_bytes) * 10
            && disk.available_bytes <= 10 * GIB
        {
            (DiskSpaceLevel::Low, 10.0, 10 * GIB)
        } else {
            continue;
        };
        insert_capacity(
            &mut low_disk_space,
            DiskCapacityHint {
                id: disk.id.clone(),
                name: disk.name.clone(),
                mount_point: disk.mount_point.clone(),
                total_bytes: disk.total_bytes,
                available_bytes: disk.available_bytes,
                available_percent,
                level,
                threshold_percent,
                threshold_bytes,
                source: MetricSource::VolumeCapacity,
                read_only: disk.read_only,
                removable: disk.removable,
                help: HelpAction::Devices,
            },
        );
    }
    let disk_coverage = CounterCoverage {
        measured: disk_sources.values().filter(|measured| **measured).count(),
        enumerated: disk_sources.len(),
    };
    let mut gpu_value: Option<f64> = None;
    let mut measured_gpus = 0;
    let mut invalid_gpu_counter = false;
    for gpu in &snapshot.gpus {
        let usage = gpu.usage_percent.and_then(percent);
        invalid_gpu_counter |= gpu.usage_percent.is_some() && usage.is_none();
        if let Some(usage) = usage {
            measured_gpus += 1;
            gpu_value = Some(gpu_value.map_or(usage, |old| old.max(usage)));
        }
    }
    let mut top_cpu = Vec::with_capacity(TOP_CONSUMER_LIMIT);
    let mut top_memory = Vec::with_capacity(TOP_CONSUMER_LIMIT);
    let mut top_disk_io = Vec::with_capacity(TOP_CONSUMER_LIMIT);
    let mut top_gpu = Vec::with_capacity(TOP_CONSUMER_LIMIT);
    let mut exact_identity_process_count = 0;
    for process in &snapshot.processes {
        if process.pid == 0 {
            continue;
        }
        exact_identity_process_count += usize::from(
            process
                .start_time_windows_ticks
                .is_some_and(|ticks| ticks > 0),
        );
        if snapshot.cpu_ready
            && let Some(value) = percent(process.cpu_usage).filter(|value| *value > 0.0)
        {
            insert_consumer(
                &mut top_cpu,
                Candidate {
                    process,
                    measurement: MetricEvidence {
                        value: MetricValue::Percent(value),
                        source: MetricSource::ProcessCpu,
                        scope: MetricScope::ProcessWholeMachineCpu,
                    },
                },
            );
        }
        let memory = process
            .working_set_bytes
            .map(|value| (value, MetricSource::ProcessWorkingSet))
            .or_else(|| {
                (process.memory_bytes > 0)
                    .then_some((process.memory_bytes, MetricSource::ProcessMemoryBytes))
            });
        if let Some((value, source)) = memory.filter(|(value, _)| *value > 0) {
            insert_consumer(
                &mut top_memory,
                Candidate {
                    process,
                    measurement: MetricEvidence {
                        value: MetricValue::Bytes(value),
                        source,
                        scope: MetricScope::ProcessWorkingSet,
                    },
                },
            );
        }
        let io = process
            .disk_read_bytes_per_sec
            .and_then(rate)
            .zip(process.disk_write_bytes_per_sec.and_then(rate))
            .and_then(|(read, written)| rate(read + written))
            .filter(|value| *value > 0.0);
        if let Some(value) = io {
            insert_consumer(
                &mut top_disk_io,
                Candidate {
                    process,
                    measurement: MetricEvidence {
                        value: MetricValue::BytesPerSecond(value),
                        source: MetricSource::ProcessIoRates,
                        scope: MetricScope::ProcessReadWriteIo,
                    },
                },
            );
        }
        if let Some(value) = process
            .gpu_usage
            .and_then(percent)
            .filter(|value| *value > 0.0)
        {
            insert_consumer(
                &mut top_gpu,
                Candidate {
                    process,
                    measurement: MetricEvidence {
                        value: MetricValue::Percent(value),
                        source: MetricSource::ProcessGpuEngineUtilization,
                        scope: MetricScope::ProcessBusiestGpuEngine,
                    },
                },
            );
        }
    }
    ResourceOverview {
        observed_age,
        freshness,
        stale_after_millis,
        cpu: ObservedLoad::new(
            ResourceKind::Cpu,
            cpu_value,
            cpu_unavailable,
            MetricSource::GlobalCpu,
            MetricScope::WholeMachineCpu,
            CounterCoverage {
                measured: usize::from(cpu_value.is_some()),
                enumerated: 1,
            },
        ),
        memory: ObservedLoad::new(
            ResourceKind::Memory,
            memory_value,
            if snapshot.memory_total == 0 {
                MetricAvailability::MissingCounter
            } else {
                MetricAvailability::InvalidCounter
            },
            MetricSource::SystemMemory,
            MetricScope::PhysicalMemory,
            CounterCoverage {
                measured: usize::from(memory_value.is_some()),
                enumerated: 1,
            },
        ),
        disk: ObservedLoad::new(
            ResourceKind::Disk,
            disk_value,
            if snapshot.disks.is_empty() {
                MetricAvailability::NoDevice
            } else if invalid_disk_counter {
                MetricAvailability::InvalidCounter
            } else {
                MetricAvailability::MissingCounter
            },
            MetricSource::VolumeActiveTime,
            MetricScope::BusiestMeasuredVolume,
            disk_coverage,
        ),
        gpu: ObservedLoad::new(
            ResourceKind::Gpu,
            gpu_value,
            if snapshot.gpus.is_empty() {
                MetricAvailability::NoDevice
            } else if invalid_gpu_counter {
                MetricAvailability::InvalidCounter
            } else {
                MetricAvailability::MissingCounter
            },
            MetricSource::GpuEngineUtilization,
            MetricScope::BusiestMeasuredGpuEngine,
            CounterCoverage {
                measured: measured_gpus,
                enumerated: snapshot.gpus.len(),
            },
        ),
        memory_capacity,
        process_count: snapshot.processes.len(),
        exact_identity_process_count,
        top_cpu: finish_consumers(top_cpu),
        top_memory: finish_consumers(top_memory),
        top_disk_io: finish_consumers(top_disk_io),
        top_gpu: finish_consumers(top_gpu),
        low_disk_space,
        disk_capacity_coverage: CounterCoverage {
            measured: capacity_sources.len(),
            enumerated: disk_sources.len(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuSample;
    use crate::monitor::DiskSample;

    fn sample() -> SystemSnapshot {
        SystemSnapshot {
            cpu_ready: true,
            cpu_usage: 25.0,
            logical_cpus: 8,
            memory_total: 100 * GIB,
            memory_used: 40 * GIB,
            memory_available: 60 * GIB,
            sample_interval_secs: 1.0,
            ..Default::default()
        }
    }
    fn process(name: &str, pid: u32, cpu: f32) -> ProcessSample {
        ProcessSample {
            pid,
            name: name.into(),
            cpu_usage: cpu,
            start_time_windows_ticks: Some(116_444_737_000_000_000 + u64::from(pid)),
            ..Default::default()
        }
    }
    fn disk(id: &str, total: u64, available: u64) -> DiskSample {
        DiskSample {
            id: id.into(),
            name: id.into(),
            mount_point: id.into(),
            total_bytes: total,
            available_bytes: available,
            ..Default::default()
        }
    }
    fn overview(snapshot: &SystemSnapshot) -> ResourceOverview {
        build_overview(snapshot, SnapshotObservedAge::new(Duration::ZERO, false))
    }

    #[test]
    fn missing_data_is_not_idle_or_healthy() {
        let summary = overview(&SystemSnapshot::default());
        assert_eq!(summary.cpu.availability, MetricAvailability::WarmingUp);
        assert_eq!(
            summary.memory.availability,
            MetricAvailability::MissingCounter
        );
        assert_eq!(summary.disk.availability, MetricAvailability::NoDevice);
        assert_eq!(summary.gpu.availability, MetricAvailability::NoDevice);
        for load in [&summary.cpu, &summary.memory, &summary.disk, &summary.gpu] {
            assert_eq!(load.level, ObservedLoadLevel::Unavailable);
            assert!(load.measurement.is_none());
        }
        assert!(summary.memory_capacity.is_none());
        assert!(summary.top_cpu.is_empty());
    }
    #[test]
    fn inclusive_load_thresholds_and_distinct_memory_threshold() {
        let mut sample = sample();
        for (value, expected) in [
            (79.0, ObservedLoadLevel::BelowHigh),
            (80.0, ObservedLoadLevel::High),
            (94.0, ObservedLoadLevel::High),
            (95.0, ObservedLoadLevel::VeryHigh),
            (100.0, ObservedLoadLevel::VeryHigh),
        ] {
            sample.cpu_usage = value;
            assert_eq!(overview(&sample).cpu.level, expected);
        }
        for (used, expected) in [
            (84, ObservedLoadLevel::BelowHigh),
            (85, ObservedLoadLevel::High),
            (95, ObservedLoadLevel::VeryHigh),
        ] {
            sample.memory_used = used * GIB;
            assert_eq!(overview(&sample).memory.level, expected);
        }
    }
    #[test]
    fn invalid_counters_are_unavailable_and_serializable() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 100.01] {
            let mut sample = sample();
            sample.cpu_usage = invalid;
            sample.gpus = vec![GpuSample {
                usage_percent: Some(invalid),
                ..Default::default()
            }];
            sample.disks = vec![DiskSample {
                active_percent: Some(invalid),
                ..Default::default()
            }];
            sample.processes = vec![ProcessSample {
                gpu_usage: Some(invalid),
                ..process("invalid", 10, invalid)
            }];
            let summary = overview(&sample);
            assert_eq!(summary.cpu.availability, MetricAvailability::InvalidCounter);
            assert_eq!(
                summary.disk.availability,
                MetricAvailability::InvalidCounter
            );
            assert_eq!(summary.gpu.availability, MetricAvailability::InvalidCounter);
            assert!(summary.top_cpu.is_empty() && summary.top_gpu.is_empty());
            serde_json::to_string(&summary).expect("finite optional evidence");
        }
    }
    #[test]
    fn measured_zero_is_available_without_filling_idle_rankings() {
        let mut sample = sample();
        sample.cpu_usage = 0.0;
        sample.processes = vec![process("idle", 10, 0.0)];
        sample.gpus = vec![GpuSample {
            usage_percent: Some(0.0),
            ..Default::default()
        }];
        sample.disks = vec![DiskSample {
            active_percent: Some(0.0),
            ..Default::default()
        }];
        let summary = overview(&sample);
        assert_eq!(summary.cpu.percent(), Some(0.0));
        assert_eq!(summary.disk.percent(), Some(0.0));
        assert_eq!(summary.gpu.percent(), Some(0.0));
        assert_eq!(summary.gpu.availability, MetricAvailability::Available);
        assert!(summary.top_cpu.is_empty());
    }
    #[test]
    fn cpu_is_not_normalized_twice_and_warmup_does_not_rank() {
        let mut sample = sample();
        sample.processes = vec![process("one core", 10, 12.5), process("Idle", 0, 100.0)];
        let summary = overview(&sample);
        assert_eq!(summary.top_cpu.len(), 1);
        assert_eq!(
            summary.top_cpu[0].measurement.value,
            MetricValue::Percent(12.5)
        );
        assert_eq!(
            summary.top_cpu[0].measurement.scope,
            MetricScope::ProcessWholeMachineCpu
        );
        sample.cpu_ready = false;
        assert!(overview(&sample).top_cpu.is_empty());
    }
    #[test]
    fn gpu_and_disk_use_busiest_measured_source_with_partial_coverage() {
        let mut sample = sample();
        sample.gpus = vec![
            GpuSample {
                usage_percent: Some(30.0),
                ..Default::default()
            },
            GpuSample {
                usage_percent: Some(75.0),
                ..Default::default()
            },
            GpuSample::default(),
        ];
        sample.disks = vec![
            DiskSample {
                id: "A".into(),
                active_percent: Some(20.0),
                ..Default::default()
            },
            DiskSample {
                id: "A".into(),
                active_percent: Some(20.0),
                ..Default::default()
            },
            DiskSample {
                id: "B".into(),
                active_percent: Some(60.0),
                ..Default::default()
            },
            disk("C", GIB, GIB),
        ];
        let summary = overview(&sample);
        assert_eq!(summary.gpu.percent(), Some(75.0));
        assert_eq!(
            summary.gpu.coverage,
            CounterCoverage {
                measured: 2,
                enumerated: 3
            }
        );
        assert_eq!(summary.disk.percent(), Some(60.0));
        assert_eq!(
            summary.disk.coverage,
            CounterCoverage {
                measured: 2,
                enumerated: 3
            }
        );
    }
    #[test]
    fn ties_are_deterministic_and_main_source_order_stays_unchanged() {
        let mut sample = sample();
        sample.processes = vec![
            process("zeta", 1, 30.0),
            process("Beta", 8, 30.0),
            process("alpha", 3, 30.0),
            process("ALPHA", 2, 30.0),
            process("high", 9, 50.0),
        ];
        let before: Vec<_> = sample.processes.iter().map(|p| p.pid).collect();
        let summary = overview(&sample);
        assert_eq!(
            summary.top_cpu.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![9, 2, 3]
        );
        assert_eq!(
            sample.processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
            before
        );
        sample.processes.reverse();
        assert_eq!(overview(&sample).top_cpu, summary.top_cpu);
    }
    #[test]
    fn missing_identity_is_preserved_and_birth_time_distinguishes_pid_reuse() {
        let mut sample = sample();
        sample.processes = vec![process("protected", 4, 20.0)];
        sample.processes[0].start_time_windows_ticks = None;
        let summary = overview(&sample);
        assert_eq!(summary.top_cpu[0].pid, 4);
        assert_eq!(summary.top_cpu[0].creation_time_windows_ticks, None);
        assert!(summary.top_cpu[0].exact_identity().is_none());
        assert_eq!(summary.exact_identity_process_count, 0);
        sample.processes[0].start_time_windows_ticks = Some(0);
        assert!(overview(&sample).top_cpu[0].exact_identity().is_none());
        sample.processes[0].start_time_windows_ticks = Some(123);
        let original = overview(&sample).top_cpu[0].exact_identity().unwrap();
        sample.processes[0].start_time_windows_ticks = Some(124);
        assert_ne!(
            overview(&sample).top_cpu[0].exact_identity().unwrap(),
            original
        );
    }
    #[test]
    fn large_byte_values_rank_and_export_as_exact_integers() {
        let mut sample = sample();
        sample.processes = vec![process("alpha", 10, 0.0), process("zeta", 11, 0.0)];
        sample.processes[0].working_set_bytes = Some(u64::MAX - 1);
        sample.processes[1].working_set_bytes = Some(u64::MAX);
        let summary = overview(&sample);
        assert_eq!(summary.top_memory[0].pid, 11);
        assert_eq!(
            summary.top_memory[0].measurement.value,
            MetricValue::Bytes(u64::MAX)
        );
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(
            json["top_memory"][0]["measurement"]["value"]["bytes"].as_u64(),
            Some(u64::MAX)
        );
    }
    #[test]
    fn unknown_memory_is_not_zero_and_known_zero_overrides_legacy_fallback() {
        let mut sample = sample();
        sample.processes = vec![
            process("unknown", 10, 0.0),
            process("fallback", 11, 0.0),
            process("known zero", 12, 0.0),
        ];
        sample.processes[1].memory_bytes = 512;
        sample.processes[2].working_set_bytes = Some(0);
        sample.processes[2].memory_bytes = 1024;
        let summary = overview(&sample);
        assert_eq!(summary.top_memory.len(), 1);
        assert_eq!(summary.top_memory[0].pid, 11);
        assert_eq!(
            summary.top_memory[0].measurement.source,
            MetricSource::ProcessMemoryBytes
        );
    }
    #[test]
    fn io_requires_both_rates_and_rejects_negative_overflow_and_nan() {
        let mut sample = sample();
        sample.processes = vec![
            ProcessSample {
                disk_read_bytes_per_sec: Some(10.0),
                disk_write_bytes_per_sec: Some(20.0),
                ..process("known", 10, 0.0)
            },
            ProcessSample {
                disk_read_bytes_per_sec: Some(1_000.0),
                disk_write_bytes_per_sec: None,
                ..process("partial", 11, 0.0)
            },
            ProcessSample {
                disk_read_bytes_per_sec: Some(f64::MAX),
                disk_write_bytes_per_sec: Some(f64::MAX),
                ..process("overflow", 12, 0.0)
            },
            ProcessSample {
                disk_read_bytes_per_sec: Some(-1.0),
                disk_write_bytes_per_sec: Some(10.0),
                ..process("invalid", 13, 0.0)
            },
            ProcessSample {
                disk_read_bytes_per_sec: Some(f64::NAN),
                disk_write_bytes_per_sec: Some(10.0),
                ..process("nan", 14, 0.0)
            },
        ];
        let summary = overview(&sample);
        assert_eq!(summary.top_disk_io.len(), 1);
        assert_eq!(
            summary.top_disk_io[0].measurement.value,
            MetricValue::BytesPerSecond(30.0)
        );
    }
    #[test]
    fn pause_and_receipt_age_do_not_change_load_classification() {
        let sample = sample();
        let current = build_overview(
            &sample,
            SnapshotObservedAge {
                age_millis: Some(5_000),
                paused: false,
            },
        );
        let delayed = build_overview(
            &sample,
            SnapshotObservedAge {
                age_millis: Some(5_001),
                paused: false,
            },
        );
        let paused = build_overview(
            &sample,
            SnapshotObservedAge {
                age_millis: Some(100_000),
                paused: true,
            },
        );
        let unknown = build_overview(&sample, SnapshotObservedAge::default());
        assert_eq!(current.freshness, SnapshotFreshness::Current);
        assert_eq!(delayed.freshness, SnapshotFreshness::Delayed);
        assert_eq!(paused.freshness, SnapshotFreshness::Paused);
        assert_eq!(unknown.freshness, SnapshotFreshness::Unknown);
        assert_eq!(current.cpu, delayed.cpu);
        assert_eq!(delayed.cpu, paused.cpu);
        assert_eq!(stale_after_millis(3.0), 9_000);
        assert_eq!(stale_after_millis(f64::NAN), 5_000);
    }
    #[test]
    fn low_capacity_requires_both_fraction_and_absolute_boundaries() {
        let mut sample = sample();
        sample.disks = vec![
            disk("low exact", 100 * GIB, 10 * GIB),
            disk("very low exact", 40 * GIB, 2 * GIB),
            disk("large but plentiful", 10_000 * GIB, 100 * GIB),
            disk("small but mostly free", GIB, GIB / 2),
        ];
        let summary = overview(&sample);
        assert_eq!(summary.low_disk_space.len(), 2);
        assert_eq!(summary.low_disk_space[0].id, "very low exact");
        assert_eq!(summary.low_disk_space[0].level, DiskSpaceLevel::VeryLow);
        assert_eq!(summary.low_disk_space[1].level, DiskSpaceLevel::Low);
        assert_eq!(summary.low_disk_space[1].threshold_bytes, 10 * GIB);
        assert_eq!(
            summary.disk_capacity_coverage,
            CounterCoverage {
                measured: 4,
                enumerated: 4
            }
        );
    }
    #[test]
    fn capacity_hints_are_bounded_deduplicated_and_deterministic() {
        let mut sample = sample();
        sample.disks = vec![
            disk("z", 10 * GIB, 0),
            disk("A", 10 * GIB, 0),
            disk("A", 10 * GIB, 0),
            disk("c", 10 * GIB, 0),
            disk("b", 10 * GIB, 0),
        ];
        let summary = overview(&sample);
        assert_eq!(
            summary
                .low_disk_space
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "b", "c"]
        );
        assert_eq!(
            summary.disk_capacity_coverage,
            CounterCoverage {
                measured: 4,
                enumerated: 4
            }
        );
        sample.disks.reverse();
        assert_eq!(overview(&sample).low_disk_space, summary.low_disk_space);
    }
    #[test]
    fn invalid_capacity_is_not_inferred_or_clamped() {
        let mut sample = sample();
        sample.memory_used = sample.memory_total + 1;
        sample.disks = vec![
            disk("zero total", 0, 0),
            disk("impossible free", GIB, 2 * GIB),
        ];
        let summary = overview(&sample);
        assert!(summary.memory_capacity.is_none());
        assert_eq!(
            summary.memory.availability,
            MetricAvailability::InvalidCounter
        );
        assert!(summary.low_disk_space.is_empty());
        assert_eq!(summary.disk_capacity_coverage.measured, 0);
        sample.memory_used = GIB;
        sample.memory_available = sample.memory_total + 1;
        assert_eq!(
            overview(&sample).memory_capacity.unwrap().available_bytes,
            None
        );
    }
}

//! Windows GPU telemetry from DXGI and WDDM performance counters.
//!
//! All adapter totals come from `GPU Adapter Memory`. DXGI's
//! `QueryVideoMemoryInfo::CurrentUsage` describes *this process*, so it is kept
//! separately and must never be displayed as whole-adapter VRAM usage.
//! GPU utilization is the busiest engine after summing its process instances,
//! following <https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/>.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory1,
};
use windows::core::Interface;

#[derive(Clone, Debug)]
pub struct GpuEngineSample {
    pub name: String,
    pub usage_percent: f32,
}

#[derive(Clone, Debug)]
pub struct GpuSample {
    /// DXGI enumeration order, not Task Manager's PCI-order GPU number.
    pub index: usize,
    pub name: String,
    pub luid: u64,
    pub usage_percent: Option<f32>,
    pub dedicated_memory_used: Option<u64>,
    pub dedicated_memory_total: u64,
    pub shared_memory_used: Option<u64>,
    pub shared_memory_total: u64,
    /// Vendor sensor APIs are required for temperatures; unavailable is None.
    pub temperature_celsius: Option<f32>,
    pub engines: Vec<GpuEngineSample>,
    pub status: String,
    /// DXGI budget and usage apply to SystemMonitorX only, not all processes.
    pub monitor_local_memory_budget: Option<u64>,
    pub monitor_local_memory_used: Option<u64>,
}

struct Adapter {
    name: String,
    luid: u64,
    dedicated_memory_total: u64,
    shared_memory_total: u64,
    adapter3: Option<IDXGIAdapter3>,
}

pub struct GpuMonitor {
    adapters: Vec<Adapter>,
    query: Option<PdhQuery>,
    process_usage: HashMap<u32, f32>,
    process_memory: HashMap<u32, u64>,
    last_device_scan: Instant,
}

impl Default for GpuMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuMonitor {
    /// Construct and sample this on the background collector thread. No sleep,
    /// subprocess, elevated privilege or vendor driver is needed.
    pub fn new() -> Self {
        Self {
            adapters: enumerate_adapters(),
            query: PdhQuery::new(),
            process_usage: HashMap::new(),
            process_memory: HashMap::new(),
            last_device_scan: Instant::now(),
        }
    }

    /// Percent of the busiest engine across all GPUs for each measured PID.
    /// Missing PIDs have no valid counter sample and must be displayed as N/A.
    pub fn process_usage(&self) -> &HashMap<u32, f32> {
        &self.process_usage
    }

    /// Dedicated plus shared bytes for each measured PID across all adapters.
    /// Cross-process shared allocations can appear in several PID totals, so
    /// these values must not be summed to calculate whole-adapter usage.
    pub fn process_memory(&self) -> &HashMap<u32, u64> {
        &self.process_memory
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        if self.last_device_scan.elapsed() >= Duration::from_secs(30) {
            self.adapters = enumerate_adapters();
            if self.query.is_none() {
                self.query = PdhQuery::new();
            }
            self.last_device_scan = Instant::now();
        }

        // Never retain a previous PID's values after a failed collection.
        self.process_usage.clear();
        self.process_memory.clear();
        let counters = self.query.as_mut().map(PdhQuery::sample);
        let aggregated = counters
            .as_ref()
            .map(aggregate_counters)
            .unwrap_or_default();
        self.process_usage.clone_from(&aggregated.process_usage);
        self.process_memory.clone_from(&aggregated.process_memory);

        self.adapters
            .iter()
            .enumerate()
            .map(|(index, adapter)| {
                let mut engines: Vec<_> = aggregated
                    .engines
                    .iter()
                    .filter(|((luid, _, _), _)| *luid == adapter.luid)
                    .map(|((_, physical, id), (kind, usage))| GpuEngineSample {
                        name: if *physical == 0 {
                            format!("{kind} · {id}")
                        } else {
                            format!("{kind} · {id} · 节点 {physical}")
                        },
                        usage_percent: (*usage as f32).clamp(0.0, 100.0),
                    })
                    .collect();
                engines.sort_by(|left, right| right.usage_percent.total_cmp(&left.usage_percent));
                let usage_percent = engines.first().map(|engine| engine.usage_percent);
                let dedicated_memory_used = aggregated.dedicated.get(&adapter.luid).copied();
                let shared_memory_used = aggregated.shared.get(&adapter.luid).copied();
                let local_info = adapter.adapter3.as_ref().and_then(|adapter3| {
                    let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                    // SAFETY: adapter3 is a live COM interface; info is valid
                    // writable storage for the call. Node zero exists on all
                    // successfully enumerated adapters.
                    unsafe {
                        adapter3
                            .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
                            .ok()
                            .map(|()| info)
                    }
                });
                let status = match counters.as_ref() {
                    None => "GPU 性能计数器不可用；需要 WDDM 2.0+ 驱动".into(),
                    Some(data) if data.error.is_some() => {
                        format!("GPU 采样失败：0x{:08X}", data.error.unwrap_or_default())
                    }
                    Some(data) if !data.engine_ready => "GPU 正在建立采样基线".into(),
                    _ if usage_percent.is_none() => {
                        "此 GPU 没有有效引擎样本；检查 WDDM 驱动".into()
                    }
                    _ if dedicated_memory_used.is_none() || shared_memory_used.is_none() => {
                        "引擎正常；部分显存计数器不可用".into()
                    }
                    _ => "WDDM / DXGI 实时采样".into(),
                };
                GpuSample {
                    index,
                    name: adapter.name.clone(),
                    luid: adapter.luid,
                    usage_percent,
                    dedicated_memory_used,
                    dedicated_memory_total: adapter.dedicated_memory_total,
                    shared_memory_used,
                    shared_memory_total: adapter.shared_memory_total,
                    temperature_celsius: None,
                    engines,
                    status,
                    monitor_local_memory_budget: local_info.as_ref().map(|info| info.Budget),
                    monitor_local_memory_used: local_info.as_ref().map(|info| info.CurrentUsage),
                }
            })
            .collect()
    }
}

fn enumerate_adapters() -> Vec<Adapter> {
    // SAFETY: DXGI creates an owned factory interface; windows-rs releases all
    // factory/adapter COM references on drop. DXGI needs no COM apartment init.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut adapters = Vec::new();
    for ordinal in 0..64 {
        // SAFETY: factory remains alive for both operations.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(ordinal) }) else {
            break;
        };
        let Ok(description) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        // Indirect display drivers (remote desktop, virtual monitors) can
        // expose several DXGI aliases with the physical GPU's same name and
        // capacities. The kernel's adapter type identifies them accurately;
        // name-based de-duplication would hide real identical-model GPUs.
        if is_indirect_display_adapter(description.AdapterLuid) {
            continue;
        }
        let name_len = description
            .Description
            .iter()
            .position(|ch| *ch == 0)
            .unwrap_or(description.Description.len());
        adapters.push(Adapter {
            name: String::from_utf16_lossy(&description.Description[..name_len]),
            luid: ((description.AdapterLuid.HighPart as u32 as u64) << 32)
                | description.AdapterLuid.LowPart as u64,
            dedicated_memory_total: description.DedicatedVideoMemory as u64,
            shared_memory_total: description.SharedSystemMemory as u64,
            adapter3: adapter.cast::<IDXGIAdapter3>().ok(),
        });
    }
    adapters
}

#[repr(C)]
struct D3dOpenAdapter {
    luid: LUID,
    handle: u32,
}

#[repr(C)]
struct D3dQueryAdapter {
    handle: u32,
    kind: i32,
    data: *mut std::ffi::c_void,
    size: u32,
}

#[repr(C)]
struct D3dAdapterHandle {
    handle: u32,
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTOpenAdapterFromLuid(adapter: *mut D3dOpenAdapter) -> i32;
    fn D3DKMTQueryAdapterInfo(query: *const D3dQueryAdapter) -> i32;
    fn D3DKMTCloseAdapter(adapter: *const D3dAdapterHandle) -> i32;
}

impl Drop for D3dAdapterHandle {
    fn drop(&mut self) {
        // SAFETY: this handle is owned after a successful open and released
        // exactly once, including on an early return from the query.
        unsafe { D3DKMTCloseAdapter(self) };
    }
}

fn is_indirect_display_adapter(luid: LUID) -> bool {
    let mut open = D3dOpenAdapter { luid, handle: 0 };
    // SAFETY: open is valid in/out storage. NTSTATUS zero is STATUS_SUCCESS.
    if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) } != 0 {
        return false;
    }
    let adapter = D3dAdapterHandle {
        handle: open.handle,
    };
    let mut adapter_type = 0_u32;
    let query = D3dQueryAdapter {
        handle: adapter.handle,
        kind: 15, // KMTQAITYPE_ADAPTERTYPE
        data: (&mut adapter_type as *mut u32).cast(),
        size: size_of::<u32>() as u32,
    };
    // SAFETY: adapter is live, and data refers to a writable u32 matching the
    // documented D3DKMT_ADAPTERTYPE union. IndirectDisplayDevice is bit 6:
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_adaptertype
    unsafe { D3DKMTQueryAdapterInfo(&query) == 0 && adapter_type & (1 << 6) != 0 }
}

// Small PDH ABI layer. Query handles own every counter added to them, and
// PdhCloseQuery releases both query and counters. Unlike localized names,
// PdhAddEnglishCounterW works on Chinese and English Windows installations.
const PDH_MORE_DATA: u32 = 0x8000_07d2;
const PDH_FMT_DOUBLE: u32 = 0x0200;
const PDH_FMT_LARGE: u32 = 0x0400;
const PDH_FMT_NOSCALE: u32 = 0x1000;
const PDH_FMT_NOCAP100: u32 = 0x8000;

#[repr(C)]
#[derive(Clone, Copy)]
union PdhValueUnion {
    double: f64,
    large: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PdhFormattedValue {
    status: u32,
    value: PdhValueUnion,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PdhFormattedItem {
    name: *const u16,
    value: PdhFormattedValue,
}

#[link(name = "pdh")]
unsafe extern "system" {
    fn PdhOpenQueryW(source: *const u16, user_data: usize, query: *mut isize) -> u32;
    fn PdhCloseQuery(query: isize) -> u32;
    fn PdhAddEnglishCounterW(
        query: isize,
        path: *const u16,
        user_data: usize,
        counter: *mut isize,
    ) -> u32;
    fn PdhCollectQueryData(query: isize) -> u32;
    fn PdhGetFormattedCounterArrayW(
        counter: isize,
        format: u32,
        bytes: *mut u32,
        item_count: *mut u32,
        items: *mut PdhFormattedItem,
    ) -> u32;
}

struct PdhQuery {
    handle: isize,
    engine: Option<isize>,
    adapter_dedicated: Option<isize>,
    adapter_shared: Option<isize>,
    process_dedicated: Option<isize>,
    process_shared: Option<isize>,
    previous_collection: Option<Instant>,
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        // SAFETY: handle was obtained from PdhOpenQueryW and is owned here.
        unsafe { PdhCloseQuery(self.handle) };
    }
}

impl PdhQuery {
    fn new() -> Option<Self> {
        let mut handle = 0;
        // SAFETY: null means live local counters; handle points to valid storage.
        if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut handle) } != 0 {
            return None;
        }
        let mut query = Self {
            handle,
            engine: None,
            adapter_dedicated: None,
            adapter_shared: None,
            process_dedicated: None,
            process_shared: None,
            previous_collection: None,
        };
        query.engine = query.add(r"\GPU Engine(*)\Utilization Percentage");
        query.adapter_dedicated = query.add(r"\GPU Adapter Memory(*)\Dedicated Usage");
        query.adapter_shared = query.add(r"\GPU Adapter Memory(*)\Shared Usage");
        query.process_dedicated = query.add(r"\GPU Process Memory(*)\Dedicated Usage");
        query.process_shared = query.add(r"\GPU Process Memory(*)\Shared Usage");
        // Prime rate counters without sleeping on the collector/UI thread.
        // SAFETY: the query and its counter handles are live.
        if unsafe { PdhCollectQueryData(handle) } == 0 {
            query.previous_collection = Some(Instant::now());
        }
        Some(query)
    }

    fn add(&self, path: &str) -> Option<isize> {
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut counter = 0;
        // SAFETY: path is NUL terminated and out pointer is valid; this query
        // owns the counter until its Drop closes the query.
        (unsafe { PdhAddEnglishCounterW(self.handle, path.as_ptr(), 0, &mut counter) } == 0)
            .then_some(counter)
    }

    fn sample(&mut self) -> CounterSnapshot {
        let now = Instant::now();
        let engine_ready = self
            .previous_collection
            .is_some_and(|previous| now.duration_since(previous) >= Duration::from_millis(200));
        // SAFETY: self owns a live query.
        let status = unsafe { PdhCollectQueryData(self.handle) };
        if status != 0 {
            self.previous_collection = None;
            return CounterSnapshot {
                error: Some(status),
                ..Default::default()
            };
        }
        self.previous_collection = Some(now);
        CounterSnapshot {
            engine_ready,
            engines: engine_ready
                .then(|| self.engine.and_then(|counter| read_counter(counter, true)))
                .flatten(),
            adapter_dedicated: self
                .adapter_dedicated
                .and_then(|counter| read_counter(counter, false)),
            adapter_shared: self
                .adapter_shared
                .and_then(|counter| read_counter(counter, false)),
            process_dedicated: self
                .process_dedicated
                .and_then(|counter| read_counter(counter, false)),
            process_shared: self
                .process_shared
                .and_then(|counter| read_counter(counter, false)),
            error: None,
        }
    }
}

#[derive(Default)]
struct CounterSnapshot {
    engine_ready: bool,
    engines: Option<Vec<CounterValue>>,
    adapter_dedicated: Option<Vec<CounterValue>>,
    adapter_shared: Option<Vec<CounterValue>>,
    process_dedicated: Option<Vec<CounterValue>>,
    process_shared: Option<Vec<CounterValue>>,
    error: Option<u32>,
}

struct CounterValue {
    name: String,
    value: f64,
}

fn read_counter(counter: isize, percent: bool) -> Option<Vec<CounterValue>> {
    let format = if percent {
        PDH_FMT_DOUBLE | PDH_FMT_NOCAP100
    } else {
        PDH_FMT_LARGE | PDH_FMT_NOSCALE
    };
    // Counter instances can change between size and data calls; retry from
    // zero-sized sizing calls as required by the PDH API, with a hard bound.
    for _ in 0..3 {
        let mut bytes = 0;
        let mut count = 0;
        // SAFETY: null data with zero size is the documented sizing operation.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut bytes,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if status == 0 && count == 0 {
            return Some(Vec::new());
        }
        if status != PDH_MORE_DATA || bytes == 0 || bytes > 32 * 1024 * 1024 {
            return None;
        }
        // u64 storage guarantees 8-byte alignment for the Windows PDH union.
        let mut buffer = vec![0_u64; (bytes as usize).div_ceil(8)];
        let allocated_bytes = buffer.len() * 8;
        bytes = allocated_bytes as u32;
        // SAFETY: buffer is aligned and writable for exactly bytes bytes.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut bytes,
                &mut count,
                buffer.as_mut_ptr().cast(),
            )
        };
        if status == PDH_MORE_DATA {
            continue;
        }
        if status != 0 || count as usize > allocated_bytes / size_of::<PdhFormattedItem>() {
            return None;
        }
        let start = buffer.as_ptr() as usize;
        let end = start + allocated_bytes;
        let mut values = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            // SAFETY: returned count was checked against allocated storage.
            let item = unsafe { *buffer.as_ptr().cast::<PdhFormattedItem>().add(index) };
            if item.value.status > 1 {
                continue; // PDH_CSTATUS_VALID_DATA or PDH_CSTATUS_NEW_DATA only.
            }
            let name_address = item.name as usize;
            if name_address < start || name_address >= end || !name_address.is_multiple_of(2) {
                continue;
            }
            // SAFETY: PDH places instance names inside the supplied buffer;
            // the address and length are bounded before reading UTF-16.
            let name_storage = unsafe {
                std::slice::from_raw_parts(item.name, ((end - name_address) / 2).min(2048))
            };
            let Some(name_len) = name_storage.iter().position(|ch| *ch == 0) else {
                continue;
            };
            // SAFETY: PDH populated the union variant requested by format.
            let value = unsafe {
                if percent {
                    item.value.value.double
                } else {
                    item.value.value.large as f64
                }
            };
            if value.is_finite() && value >= 0.0 {
                values.push(CounterValue {
                    name: String::from_utf16_lossy(&name_storage[..name_len]),
                    value,
                });
            }
        }
        return Some(values);
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CounterIdentity {
    pid: Option<u32>,
    luid: u64,
    physical: u32,
    engine: Option<u32>,
    engine_type: String,
}

fn parse_instance(name: &str) -> Option<CounterIdentity> {
    let parts: Vec<_> = name.split('_').collect();
    let luid_index = parts
        .iter()
        .position(|part| part.eq_ignore_ascii_case("luid"))?;
    let parse_hex = |text: &str| {
        u32::from_str_radix(
            text.strip_prefix("0x")
                .or_else(|| text.strip_prefix("0X"))?,
            16,
        )
        .ok()
    };
    let high = parse_hex(parts.get(luid_index + 1)?)?;
    let low = parse_hex(parts.get(luid_index + 2)?)?;
    let marker_number = |marker: &str| -> Option<u32> {
        let index = parts
            .iter()
            .position(|part| part.eq_ignore_ascii_case(marker))?;
        parts.get(index + 1)?.split('#').next()?.parse().ok()
    };
    let physical = marker_number("phys")?;
    let engine_type = name
        .split_once("_engtype_")
        .map(|(_, kind)| kind.split('#').next().unwrap_or(kind).to_owned())
        .unwrap_or_else(|| "Engine".into());
    Some(CounterIdentity {
        pid: marker_number("pid"),
        luid: ((high as u64) << 32) | low as u64,
        physical,
        engine: marker_number("eng"),
        engine_type,
    })
}

#[derive(Default)]
struct AggregatedCounters {
    engines: BTreeMap<(u64, u32, u32), (String, f64)>,
    dedicated: HashMap<u64, u64>,
    shared: HashMap<u64, u64>,
    process_usage: HashMap<u32, f32>,
    process_memory: HashMap<u32, u64>,
}

fn aggregate_counters(snapshot: &CounterSnapshot) -> AggregatedCounters {
    let mut result = AggregatedCounters::default();
    let mut process_engines: HashMap<(u32, u64, u32, u32), f64> = HashMap::new();
    for counter in snapshot.engines.iter().flatten() {
        if !counter.value.is_finite() || counter.value < 0.0 {
            continue;
        }
        let Some(instance) = parse_instance(&counter.name) else {
            continue;
        };
        let (Some(pid), Some(engine)) = (instance.pid, instance.engine) else {
            continue;
        };
        let key = (instance.luid, instance.physical, engine);
        let entry = result
            .engines
            .entry(key)
            .or_insert((instance.engine_type, 0.0));
        entry.1 += counter.value;
        *process_engines
            .entry((pid, key.0, key.1, key.2))
            .or_default() += counter.value;
    }
    for ((pid, _, _, _), value) in process_engines {
        let value = (value as f32).clamp(0.0, 100.0);
        result
            .process_usage
            .entry(pid)
            .and_modify(|usage| *usage = usage.max(value))
            .or_insert(value);
    }
    result.dedicated = adapter_memory(snapshot.adapter_dedicated.as_deref());
    result.shared = adapter_memory(snapshot.adapter_shared.as_deref());

    // Only combine a PID/adapter/node when both memory counters were valid.
    let dedicated = process_memory(snapshot.process_dedicated.as_deref());
    let shared = process_memory(snapshot.process_shared.as_deref());
    for (key, dedicated_bytes) in dedicated {
        if let Some(shared_bytes) = shared.get(&key) {
            let total = result.process_memory.entry(key.0).or_default();
            *total = total.saturating_add(dedicated_bytes.saturating_add(*shared_bytes));
        }
    }
    result
}

fn adapter_memory(counters: Option<&[CounterValue]>) -> HashMap<u64, u64> {
    let mut memory: HashMap<u64, u64> = HashMap::new();
    for counter in counters.into_iter().flatten() {
        if let Some(instance) = parse_instance(&counter.name)
            && counter.value.is_finite()
            && counter.value >= 0.0
        {
            let value = memory.entry(instance.luid).or_default();
            *value = value.saturating_add(counter.value as u64);
        }
    }
    memory
}

fn process_memory(counters: Option<&[CounterValue]>) -> HashMap<(u32, u64, u32), u64> {
    let mut memory: HashMap<(u32, u64, u32), u64> = HashMap::new();
    for counter in counters.into_iter().flatten() {
        if let Some(instance) = parse_instance(&counter.name)
            && let Some(pid) = instance.pid
            && counter.value.is_finite()
            && counter.value >= 0.0
        {
            let value = memory
                .entry((pid, instance.luid, instance.physical))
                .or_default();
            *value = value.saturating_add(counter.value as u64);
        }
    }
    memory
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(name: &str, value: f64) -> CounterValue {
        CounterValue {
            name: name.into(),
            value,
        }
    }

    #[test]
    fn instance_parser_preserves_full_luid_and_physical_engine() {
        let parsed =
            parse_instance("pid_42_luid_0xFFFFFFFF_0x0012ABCD_phys_1_eng_3_engtype_VideoDecode#1")
                .expect("valid WDDM counter instance");
        assert_eq!(parsed.luid, 0xFFFF_FFFF_0012_ABCD);
        assert_eq!(parsed.pid, Some(42));
        assert_eq!(parsed.physical, 1);
        assert_eq!(parsed.engine, Some(3));
        assert_eq!(parsed.engine_type, "VideoDecode");
        assert!(parse_instance("pid_42_luid_bad_0x0012ABCD_phys_0").is_none());
        assert!(parse_instance("pid_42_eng_0_engtype_3D").is_none());
    }

    #[test]
    fn utilization_sums_processes_in_same_engine_and_keeps_gpus_separate() {
        let snapshot = CounterSnapshot {
            engine_ready: true,
            engines: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 25.0),
                value("pid_2_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 30.0),
                value("pid_1_luid_0x0_0xA_phys_0_eng_1_engtype_Copy", 40.0),
                value("pid_1_luid_0x0_0xB_phys_0_eng_0_engtype_3D", 12.0),
                value("pid_3_luid_0x0_0xA_phys_1_eng_0_engtype_3D", 20.0),
                value("pid_4_luid_0x0_0xA_phys_0_eng_2_engtype_Compute", -1.0),
            ]),
            ..Default::default()
        };
        let aggregate = aggregate_counters(&snapshot);
        assert_eq!(aggregate.engines[&(0xA, 0, 0)].1, 55.0);
        assert_eq!(aggregate.engines[&(0xA, 0, 1)].1, 40.0);
        assert_eq!(aggregate.engines[&(0xB, 0, 0)].1, 12.0);
        assert_eq!(aggregate.engines[&(0xA, 1, 0)].1, 20.0);
        assert_eq!(aggregate.process_usage[&1], 40.0);
        assert!(!aggregate.process_usage.contains_key(&4));
        let busiest = aggregate
            .engines
            .iter()
            .filter(|(key, _)| key.0 == 0xA)
            .map(|(_, value)| value.1)
            .fold(0.0_f64, f64::max);
        assert_eq!(busiest, 55.0);
    }

    #[test]
    fn adapter_memory_is_not_sum_of_process_allocations() {
        let snapshot = CounterSnapshot {
            adapter_dedicated: Some(vec![value("luid_0x0_0xA_phys_0", 4096.0)]),
            process_dedicated: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0", 3072.0),
                value("pid_2_luid_0x0_0xA_phys_0", 3072.0),
            ]),
            process_shared: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0", 1024.0),
                value("pid_2_luid_0x0_0xA_phys_0", 0.0),
            ]),
            ..Default::default()
        };
        let aggregate = aggregate_counters(&snapshot);
        assert_eq!(aggregate.dedicated[&0xA], 4096);
        assert_eq!(aggregate.process_memory[&1], 4096);
        assert_eq!(aggregate.process_memory[&2], 3072);
        assert!(!aggregate.shared.contains_key(&0xA));
    }

    #[test]
    fn missing_and_invalid_counters_do_not_become_zero_usage() {
        let aggregate = aggregate_counters(&CounterSnapshot::default());
        assert!(aggregate.engines.is_empty());
        assert!(aggregate.process_usage.is_empty());
        assert!(aggregate.dedicated.is_empty());
        let snapshot = CounterSnapshot {
            adapter_dedicated: Some(vec![
                value("luid_0x0_0xA_phys_0", f64::NAN),
                value("luid_0x0_0xB_phys_0", -5.0),
                value("luid_0x0_0xC_phys_0", 0.0),
            ]),
            ..Default::default()
        };
        let aggregate = aggregate_counters(&snapshot);
        assert!(!aggregate.dedicated.contains_key(&0xA));
        assert!(!aggregate.dedicated.contains_key(&0xB));
        assert_eq!(aggregate.dedicated.get(&0xC), Some(&0));
    }
}

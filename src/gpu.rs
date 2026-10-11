//! Windows GPU telemetry from DXGI and WDDM performance counters.
//!
//! All adapter totals come from `GPU Adapter Memory`. DXGI's
//! `QueryVideoMemoryInfo::CurrentUsage` describes *this process*, so it is kept
//! separately and must never be displayed as whole-adapter VRAM usage.
//! GPU utilization is the busiest engine after summing its process instances,
//! following <https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/>.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{c_char, c_void};
use std::fmt;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FreeLibrary, HMODULE, LUID};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory1,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::Interface;
use windows::core::{PCSTR, w};

#[derive(Clone, Debug)]
pub struct GpuEngineSample {
    pub name: String,
    pub usage_percent: f32,
}

#[derive(Clone, Debug, Default)]
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
    pub vendor_id: u32,
    pub device_id: u32,
    pub subsystem_id: u32,
    pub pci_address: Option<PciAddress>,
    /// NVIDIA driver version reported by NVML; other vendors are in inventory.
    pub driver_version: Option<String>,
    pub vbios_version: Option<String>,
    pub power_watts: Option<f32>,
    pub power_limit_watts: Option<f32>,
    /// NVML's actual fan speed percentage, including a stopped fan at zero.
    pub fan_percent: Option<u32>,
    pub core_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub pcie_generation: Option<u32>,
    pub pcie_width: Option<u32>,
    pub sensor_source: Option<String>,
    pub sensor_note: Option<String>,
    pub engines: Vec<GpuEngineSample>,
    pub status: String,
    /// DXGI budget and usage apply to SystemMonitorX only, not all processes.
    pub monitor_local_memory_budget: Option<u64>,
    pub monitor_local_memory_used: Option<u64>,
}

/// PCI bus/device/function from the exact DXGI LUID. Windows' D3DKMT query
/// omits the PCI domain, so it is only filled after an unambiguous NVML match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PciAddress {
    pub domain: Option<u32>,
    pub bus: u32,
    pub device: u32,
    pub function: u32,
}

impl fmt::Display for PciAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.domain {
            Some(domain) => write!(
                formatter,
                "{domain:04X}:{:02X}:{:02X}.{}",
                self.bus, self.device, self.function
            ),
            None => write!(
                formatter,
                "?:{:02X}:{:02X}.{}",
                self.bus, self.device, self.function
            ),
        }
    }
}

struct Adapter {
    name: String,
    luid: u64,
    dedicated_memory_total: u64,
    shared_memory_total: u64,
    vendor_id: u32,
    device_id: u32,
    subsystem_id: u32,
    pci_address: Option<PciAddress>,
    adapter3: Option<IDXGIAdapter3>,
}

pub struct GpuMonitor {
    adapters: Vec<Adapter>,
    query: Option<PdhQuery>,
    process_usage: HashMap<u32, f32>,
    process_memory: HashMap<u32, u64>,
    process_memory_note: Option<String>,
    last_device_scan: Instant,
    nvml: Option<Nvml>,
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
        let adapters = enumerate_adapters();
        let nvml = adapters
            .iter()
            .any(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID)
            .then(Nvml::new)
            .flatten();
        Self {
            adapters,
            query: PdhQuery::new(),
            process_usage: HashMap::new(),
            process_memory: HashMap::new(),
            process_memory_note: None,
            last_device_scan: Instant::now(),
            nvml,
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

    /// Explains why obviously invalid WDDM process-memory values were omitted.
    /// Adapter totals and engine utilization remain independently available.
    pub fn process_memory_note(&self) -> Option<&str> {
        self.process_memory_note.as_deref()
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        if self.last_device_scan.elapsed() >= Duration::from_secs(30) {
            self.adapters = enumerate_adapters();
            if let Some(nvml) = self.nvml.as_mut() {
                nvml.refresh_devices();
            } else if self
                .adapters
                .iter()
                .any(|adapter| adapter.vendor_id == NVIDIA_VENDOR_ID)
            {
                self.nvml = Nvml::new();
            }
            if self.query.is_none() {
                self.query = PdhQuery::new();
            }
            self.last_device_scan = Instant::now();
        }

        // Never retain a previous PID's values after a failed collection.
        self.process_usage.clear();
        self.process_memory.clear();
        self.process_memory_note = None;
        let counters = self.query.as_mut().map(PdhQuery::sample);
        // Use the same hardware LUID set for per-process and adapter views.
        // Otherwise excluded indirect-display aliases can still pollute PIDs.
        let adapter_capacities: HashMap<_, _> = self
            .adapters
            .iter()
            .map(|adapter| (adapter.luid, adapter.dedicated_memory_total))
            .collect();
        let aggregated = counters
            .as_ref()
            .map(|snapshot| aggregate_counters(snapshot, &adapter_capacities))
            .unwrap_or_default();
        self.process_usage.clone_from(&aggregated.process_usage);
        self.process_memory.clone_from(&aggregated.process_memory);
        if !aggregated.invalid_process_memory_pids.is_empty() {
            self.process_memory_note = Some(format!(
                "{} 个进程的 GPU 内存计数超出显卡容量，已显示为不可用（Windows WDDM 计数器异常）。",
                aggregated.invalid_process_memory_pids.len()
            ));
        }

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
                let mut gpu = GpuSample {
                    index,
                    name: adapter.name.clone(),
                    luid: adapter.luid,
                    usage_percent,
                    dedicated_memory_used,
                    dedicated_memory_total: adapter.dedicated_memory_total,
                    shared_memory_used,
                    shared_memory_total: adapter.shared_memory_total,
                    temperature_celsius: None,
                    vendor_id: adapter.vendor_id,
                    device_id: adapter.device_id,
                    subsystem_id: adapter.subsystem_id,
                    pci_address: adapter.pci_address,
                    engines,
                    status,
                    monitor_local_memory_budget: local_info.as_ref().map(|info| info.Budget),
                    monitor_local_memory_used: local_info.as_ref().map(|info| info.CurrentUsage),
                    ..GpuSample::default()
                };
                if adapter.vendor_id == NVIDIA_VENDOR_ID {
                    if let Some(nvml) = self.nvml.as_ref() {
                        nvml.fill_sample(adapter, &self.adapters, &mut gpu);
                    } else {
                        gpu.sensor_note = Some(
                            "NVIDIA 驱动未提供可用的 NVML；温度、功耗、风扇和频率不可用".into(),
                        );
                    }
                } else {
                    gpu.sensor_note = Some(
                        "通用 WDDM 提供利用率和显存；此设备未接入厂商温度、功耗及风扇接口".into(),
                    );
                }
                gpu
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
            vendor_id: description.VendorId,
            device_id: description.DeviceId,
            subsystem_id: description.SubSysId,
            pci_address: adapter_pci_address(description.AdapterLuid),
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

fn adapter_pci_address(luid: LUID) -> Option<PciAddress> {
    let mut open = D3dOpenAdapter { luid, handle: 0 };
    // SAFETY: open is valid in/out storage and the returned handle is owned.
    if unsafe { D3DKMTOpenAdapterFromLuid(&mut open) } != 0 {
        return None;
    }
    let adapter = D3dAdapterHandle {
        handle: open.handle,
    };
    // D3DKMT_ADAPTERADDRESS is three UINTs (bus, device, function), with no
    // PCI domain. Querying by LUID avoids confusing identical-model cards.
    let mut address = [0_u32; 3];
    let query = D3dQueryAdapter {
        handle: adapter.handle,
        kind: 6, // KMTQAITYPE_ADAPTERADDRESS
        data: address.as_mut_ptr().cast(),
        size: size_of_val(&address) as u32,
    };
    // SAFETY: the live adapter and correctly sized output buffer match the
    // documented D3DKMT_ADAPTERADDRESS ABI.
    if unsafe { D3DKMTQueryAdapterInfo(&query) } != 0
        || address[0] > 0xff
        || address[1] > 31
        || address[2] > 7
    {
        return None;
    }
    Some(PciAddress {
        domain: None,
        bus: address[0],
        device: address[1],
        function: address[2],
    })
}

const NVIDIA_VENDOR_ID: u32 = 0x10de;
type NvmlDeviceHandle = *mut c_void;
type NvmlInit = unsafe extern "C" fn() -> i32;
type NvmlShutdown = unsafe extern "C" fn() -> i32;
type NvmlDeviceCount = unsafe extern "C" fn(*mut u32) -> i32;
type NvmlDeviceByIndex = unsafe extern "C" fn(u32, *mut NvmlDeviceHandle) -> i32;
type NvmlDevicePciInfo = unsafe extern "C" fn(NvmlDeviceHandle, *mut NvmlPciInfo) -> i32;
type NvmlDeviceU32 = unsafe extern "C" fn(NvmlDeviceHandle, *mut u32) -> i32;
type NvmlDeviceClock = unsafe extern "C" fn(NvmlDeviceHandle, u32, *mut u32) -> i32;
type NvmlDeviceString = unsafe extern "C" fn(NvmlDeviceHandle, *mut c_char, u32) -> i32;
type NvmlSystemString = unsafe extern "C" fn(*mut c_char, u32) -> i32;
type NvmlDeviceTemperature = unsafe extern "C" fn(NvmlDeviceHandle, *mut NvmlTemperature) -> i32;
type NvmlDeviceTemperatureLegacy = unsafe extern "C" fn(NvmlDeviceHandle, u32, *mut u32) -> i32;

/// Version-three NVML PCI ABI, including both legacy and current bus IDs.
#[repr(C)]
struct NvmlPciInfo {
    bus_id_legacy: [c_char; 16],
    domain: u32,
    bus: u32,
    device: u32,
    pci_device_id: u32,
    pci_subsystem_id: u32,
    bus_id: [c_char; 32],
}

#[repr(C)]
struct NvmlTemperature {
    version: u32,
    sensor_type: u32,
    temperature: i32,
}

/// Own the DLL independently of function pointers. The System32-only search
/// prevents loading an unrelated nvml.dll from an application/current folder.
struct NvmlLibrary {
    handle: usize,
}

impl NvmlLibrary {
    fn load() -> Option<Self> {
        // SAFETY: the static UTF-16 name is valid and only the trusted system
        // directory is searched. No driver or software installation is done.
        let handle =
            unsafe { LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok()?;
        Some(Self {
            handle: handle.0 as usize,
        })
    }

    fn handle(&self) -> HMODULE {
        HMODULE(self.handle as *mut c_void)
    }
}

impl Drop for NvmlLibrary {
    fn drop(&mut self) {
        // SAFETY: the module owns one reference acquired by LoadLibraryExW.
        let _ = unsafe { FreeLibrary(self.handle()) };
    }
}

// Every name/signature below is taken from NVIDIA's public nvml.h. This macro
// performs a concrete function-pointer conversion, never arbitrary T/data.
macro_rules! nvml_symbol {
    ($library:expr, $name:expr, $signature:ty) => {{
        // SAFETY: fixed NUL-terminated symbol name; the precise exported C ABI
        // is specified at each use. The library outlives all saved pointers.
        unsafe {
            GetProcAddress($library.handle(), PCSTR($name.as_ptr().cast())).map(|function| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, $signature>(function)
            })
        }
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NvmlDeviceIdentity {
    address: PciAddress,
    pci_device_id: u32,
    pci_subsystem_id: u32,
}

struct NvmlDevice {
    handle: usize,
    identity: NvmlDeviceIdentity,
    vbios_version: Option<String>,
}

struct Nvml {
    // This field must remain alive until Drop has called nvmlShutdown.
    _library: NvmlLibrary,
    shutdown: NvmlShutdown,
    count: NvmlDeviceCount,
    device_by_index: NvmlDeviceByIndex,
    pci_info: NvmlDevicePciInfo,
    temperature: Option<NvmlDeviceTemperature>,
    temperature_legacy: Option<NvmlDeviceTemperatureLegacy>,
    power: Option<NvmlDeviceU32>,
    power_limit: Option<NvmlDeviceU32>,
    fan: Option<NvmlDeviceU32>,
    clock: Option<NvmlDeviceClock>,
    pcie_generation: Option<NvmlDeviceU32>,
    pcie_width: Option<NvmlDeviceU32>,
    vbios: Option<NvmlDeviceString>,
    driver_version: Option<String>,
    devices: Vec<NvmlDevice>,
}

impl Nvml {
    fn new() -> Option<Self> {
        let library = NvmlLibrary::load()?;
        let init = nvml_symbol!(library, c"nvmlInit_v2", NvmlInit)?;
        let shutdown = nvml_symbol!(library, c"nvmlShutdown", NvmlShutdown)?;
        let count = nvml_symbol!(library, c"nvmlDeviceGetCount_v2", NvmlDeviceCount)?;
        let device_by_index =
            nvml_symbol!(library, c"nvmlDeviceGetHandleByIndex_v2", NvmlDeviceByIndex)?;
        let pci_info = nvml_symbol!(library, c"nvmlDeviceGetPciInfo_v3", NvmlDevicePciInfo)?;
        // SAFETY: matching NVML function ABI; success acquires one NVML init
        // reference. The fully constructed owner releases it exactly once.
        if unsafe { init() } != 0 {
            return None;
        }
        let driver_version = nvml_symbol!(library, c"nvmlSystemGetDriverVersion", NvmlSystemString)
            .and_then(nvml_system_string);
        let mut nvml = Self {
            shutdown,
            count,
            device_by_index,
            pci_info,
            temperature: nvml_symbol!(library, c"nvmlDeviceGetTemperatureV", NvmlDeviceTemperature),
            temperature_legacy: nvml_symbol!(
                library,
                c"nvmlDeviceGetTemperature",
                NvmlDeviceTemperatureLegacy
            ),
            power: nvml_symbol!(library, c"nvmlDeviceGetPowerUsage", NvmlDeviceU32),
            power_limit: nvml_symbol!(library, c"nvmlDeviceGetPowerManagementLimit", NvmlDeviceU32),
            fan: nvml_symbol!(library, c"nvmlDeviceGetFanSpeed", NvmlDeviceU32),
            clock: nvml_symbol!(library, c"nvmlDeviceGetClockInfo", NvmlDeviceClock),
            pcie_generation: nvml_symbol!(
                library,
                c"nvmlDeviceGetCurrPcieLinkGeneration",
                NvmlDeviceU32
            ),
            pcie_width: nvml_symbol!(library, c"nvmlDeviceGetCurrPcieLinkWidth", NvmlDeviceU32),
            vbios: nvml_symbol!(library, c"nvmlDeviceGetVbiosVersion", NvmlDeviceString),
            driver_version,
            devices: Vec::new(),
            _library: library,
        };
        nvml.refresh_devices();
        Some(nvml)
    }

    fn refresh_devices(&mut self) {
        self.devices.clear();
        let mut count = 0_u32;
        // SAFETY: initialized NVML, correctly typed function and writable count.
        if unsafe { (self.count)(&mut count) } != 0 || count > 256 {
            return;
        }
        for index in 0..count {
            let mut handle = std::ptr::null_mut();
            // SAFETY: index is in the enumerated range; output is valid.
            if unsafe { (self.device_by_index)(index, &mut handle) } != 0 || handle.is_null() {
                continue;
            }
            let mut pci = NvmlPciInfo {
                bus_id_legacy: [0; 16],
                domain: 0,
                bus: 0,
                device: 0,
                pci_device_id: 0,
                pci_subsystem_id: 0,
                bus_id: [0; 32],
            };
            // SAFETY: handle was returned by NVML, v3 has exactly this struct.
            if unsafe { (self.pci_info)(handle, &mut pci) } != 0 {
                continue;
            }
            let Some(address) = nvml_c_string(&pci.bus_id)
                .as_deref()
                .and_then(parse_pci_address)
                .filter(|address| {
                    address.domain == Some(pci.domain)
                        && address.bus == pci.bus
                        && address.device == pci.device
                })
            else {
                continue;
            };
            self.devices.push(NvmlDevice {
                handle: handle as usize,
                identity: NvmlDeviceIdentity {
                    address,
                    pci_device_id: pci.pci_device_id,
                    pci_subsystem_id: pci.pci_subsystem_id,
                },
                vbios_version: self.vbios.and_then(|get| nvml_device_string(get, handle)),
            });
        }
    }

    fn fill_sample(&self, adapter: &Adapter, adapters: &[Adapter], sample: &mut GpuSample) {
        let Some(identity) = adapter_identity(adapter) else {
            sample.sensor_note = Some("无法取得此 LUID 的 PCI 位置，未绑定 NVIDIA 传感器".into());
            return;
        };
        let adapter_identities: Vec<_> = adapters.iter().filter_map(adapter_identity).collect();
        let nvml_identities: Vec<_> = self.devices.iter().map(|device| device.identity).collect();
        let Some(index) = unique_pci_match(identity, &adapter_identities, &nvml_identities) else {
            sample.sensor_note =
                Some("NVML 与 Windows PCI 标识无法唯一对应，未绑定此设备传感器".into());
            return;
        };
        let device = &self.devices[index];
        let handle = device.handle as NvmlDeviceHandle;
        sample.pci_address = Some(device.identity.address);
        sample.driver_version.clone_from(&self.driver_version);
        sample.vbios_version.clone_from(&device.vbios_version);
        sample.sensor_source = Some("NVIDIA NVML · PCI 唯一匹配".into());
        sample.temperature_celsius = self.read_temperature(handle);
        sample.power_watts = self
            .power
            .and_then(|get| nvml_u32(get, handle))
            .map(milliwatts_to_watts);
        sample.power_limit_watts = self
            .power_limit
            .and_then(|get| nvml_u32(get, handle))
            .map(milliwatts_to_watts);
        sample.fan_percent = self.fan.and_then(|get| nvml_u32(get, handle));
        sample.core_clock_mhz = self.clock.and_then(|get| nvml_clock(get, handle, 0));
        sample.memory_clock_mhz = self.clock.and_then(|get| nvml_clock(get, handle, 2));
        sample.pcie_generation = self.pcie_generation.and_then(|get| nvml_u32(get, handle));
        sample.pcie_width = self.pcie_width.and_then(|get| nvml_u32(get, handle));
        if sample.temperature_celsius.is_none()
            || sample.power_watts.is_none()
            || sample.fan_percent.is_none()
            || sample.core_clock_mhz.is_none()
            || sample.memory_clock_mhz.is_none()
        {
            sample.sensor_note = Some("NVML 驱动未提供部分传感器读数；对应项目显示为不可用".into());
        }
    }

    fn read_temperature(&self, handle: NvmlDeviceHandle) -> Option<f32> {
        if let Some(get) = self.temperature {
            let mut temperature = NvmlTemperature {
                version: size_of::<NvmlTemperature>() as u32 | (1 << 24),
                sensor_type: 0, // NVML_TEMPERATURE_GPU
                temperature: 0,
            };
            // SAFETY: initialized NVML handle and the versioned v1 temperature
            // struct from the current public API. Unsupported APIs fall back.
            if unsafe { get(handle, &mut temperature) } == 0 {
                return Some(temperature.temperature as f32);
            }
        }
        let get = self.temperature_legacy?;
        let mut temperature = 0_u32;
        // SAFETY: legacy ABI is supported on older NVIDIA drivers, sensor 0.
        (unsafe { get(handle, 0, &mut temperature) } == 0).then_some(temperature as f32)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: one successful init reference is owned, with DLL still live.
        // Module unloading happens only after this Drop body returns.
        unsafe { (self.shutdown)() };
    }
}

fn adapter_identity(adapter: &Adapter) -> Option<NvmlDeviceIdentity> {
    (adapter.vendor_id == NVIDIA_VENDOR_ID).then_some(NvmlDeviceIdentity {
        address: adapter.pci_address?,
        pci_device_id: (adapter.device_id << 16) | adapter.vendor_id,
        pci_subsystem_id: adapter.subsystem_id,
    })
}

/// Domain-less Windows addresses must be unique on both sides. Otherwise two
/// identical cards across PCI domains could silently inherit each other's data.
fn unique_pci_match(
    target: NvmlDeviceIdentity,
    adapters: &[NvmlDeviceIdentity],
    devices: &[NvmlDeviceIdentity],
) -> Option<usize> {
    let matches = |identity: &NvmlDeviceIdentity| {
        target.address.bus == identity.address.bus
            && target.address.device == identity.address.device
            && target.address.function == identity.address.function
            && target
                .address
                .domain
                .is_none_or(|domain| identity.address.domain == Some(domain))
            && target.pci_device_id == identity.pci_device_id
            && target.pci_subsystem_id == identity.pci_subsystem_id
    };
    if adapters.iter().filter(|identity| matches(identity)).count() != 1 {
        return None;
    }
    let mut candidates = devices
        .iter()
        .enumerate()
        .filter(|(_, identity)| matches(identity));
    let (index, _) = candidates.next()?;
    candidates.next().is_none().then_some(index)
}

fn parse_pci_address(value: &str) -> Option<PciAddress> {
    let (domain, rest) = value.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (device, function) = rest.split_once('.')?;
    let address = PciAddress {
        domain: Some(u32::from_str_radix(domain, 16).ok()?),
        bus: u32::from_str_radix(bus, 16).ok()?,
        device: u32::from_str_radix(device, 16).ok()?,
        function: u32::from_str_radix(function, 16).ok()?,
    };
    (address.bus <= 0xff && address.device <= 31 && address.function <= 7).then_some(address)
}

fn nvml_c_string(buffer: &[c_char]) -> Option<String> {
    // Read inside the bounded buffer only, even if a buggy driver omits NUL.
    let length = buffer.iter().position(|byte| *byte == 0)?;
    let bytes: Vec<u8> = buffer[..length].iter().map(|byte| *byte as u8).collect();
    let string = String::from_utf8_lossy(&bytes).trim().to_owned();
    (!string.is_empty()).then_some(string)
}

fn nvml_system_string(get: NvmlSystemString) -> Option<String> {
    let mut buffer = [0 as c_char; 96];
    // SAFETY: trusted NVML function with valid writable storage and length.
    if unsafe { get(buffer.as_mut_ptr(), buffer.len() as u32) } != 0 {
        return None;
    }
    nvml_c_string(&buffer)
}

fn nvml_device_string(get: NvmlDeviceString, handle: NvmlDeviceHandle) -> Option<String> {
    let mut buffer = [0 as c_char; 96];
    // SAFETY: live NVML device and valid bounded writable string storage.
    if unsafe { get(handle, buffer.as_mut_ptr(), buffer.len() as u32) } != 0 {
        return None;
    }
    nvml_c_string(&buffer)
}

fn nvml_u32(get: NvmlDeviceU32, handle: NvmlDeviceHandle) -> Option<u32> {
    let mut value = 0_u32;
    // SAFETY: trusted NVML function, live device, valid writable u32 storage.
    (unsafe { get(handle, &mut value) } == 0).then_some(value)
}

fn nvml_clock(get: NvmlDeviceClock, handle: NvmlDeviceHandle, clock: u32) -> Option<u32> {
    let mut value = 0_u32;
    // SAFETY: clock enum 0 (graphics) or 2 (memory) and valid writable storage.
    (unsafe { get(handle, clock, &mut value) } == 0).then_some(value)
}

fn milliwatts_to_watts(value: u32) -> f32 {
    value as f32 / 1000.0
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
    invalid_process_memory_pids: HashSet<u32>,
}

fn aggregate_counters(
    snapshot: &CounterSnapshot,
    adapter_capacities: &HashMap<u64, u64>,
) -> AggregatedCounters {
    let mut result = AggregatedCounters::default();
    let mut process_engines: HashMap<(u32, u64, u32, u32), f64> = HashMap::new();
    for counter in snapshot.engines.iter().flatten() {
        if !counter.value.is_finite() || counter.value < 0.0 {
            continue;
        }
        let Some(instance) = parse_instance(&counter.name) else {
            continue;
        };
        if !adapter_capacities.contains_key(&instance.luid) {
            continue;
        }
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
    result
        .dedicated
        .retain(|luid, _| adapter_capacities.contains_key(luid));
    result
        .shared
        .retain(|luid, _| adapter_capacities.contains_key(luid));

    // Only combine a PID/adapter/node when both memory counters were valid.
    let dedicated = process_memory(snapshot.process_dedicated.as_deref());
    let shared = process_memory(snapshot.process_shared.as_deref());
    for (key, dedicated_bytes) in dedicated {
        let Some(capacity) = adapter_capacities.get(&key.1) else {
            continue;
        };
        // KB4490156 documents cumulative errors in GPU Process Memory while
        // whole-adapter counters remain correct. An individual dedicated
        // allocation cannot reside beyond a reported positive VRAM capacity.
        // Do not turn this impossible value into either a clipped value or a
        // partial multi-GPU total. A zero DXGI capacity is not a usable bound.
        // https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value
        if *capacity > 0 && dedicated_bytes > *capacity {
            result.invalid_process_memory_pids.insert(key.0);
            continue;
        }
        if let Some(shared_bytes) = shared.get(&key) {
            let total = result.process_memory.entry(key.0).or_default();
            *total = total.saturating_add(dedicated_bytes.saturating_add(*shared_bytes));
        }
    }
    result
        .process_memory
        .retain(|pid, _| !result.invalid_process_memory_pids.contains(pid));
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

    fn pci_identity(address: &str) -> NvmlDeviceIdentity {
        NvmlDeviceIdentity {
            address: parse_pci_address(address).unwrap(),
            pci_device_id: 0x2c05_10de,
            pci_subsystem_id: 0x4215_1458,
        }
    }

    #[test]
    fn pci_parser_and_sensor_units_preserve_real_values() {
        let address = parse_pci_address("00000000:01:00.0").unwrap();
        assert_eq!(address.domain, Some(0));
        assert_eq!(address.bus, 1);
        assert_eq!(address.to_string(), "0000:01:00.0");
        assert!(parse_pci_address("0000:100:00.0").is_none());
        assert!(parse_pci_address("0000:01:20.0").is_none());
        assert!(parse_pci_address("0000:01:00.8").is_none());
        assert!(parse_pci_address("GeForce RTX 5070 Ti").is_none());
        assert_eq!(milliwatts_to_watts(25_250), 25.25);
        assert_eq!(milliwatts_to_watts(0), 0.0);
        assert_eq!(size_of::<NvmlPciInfo>(), 68);
        assert_eq!(size_of::<NvmlTemperature>(), 12);
    }

    #[test]
    fn identical_gpu_models_match_by_pci_instead_of_enumeration_order() {
        let first = pci_identity("0000:01:00.0");
        let second = pci_identity("0000:02:00.0");
        let adapters: Vec<_> = [first, second]
            .into_iter()
            .map(|mut identity| {
                identity.address.domain = None;
                identity
            })
            .collect();
        let devices = [second, first];
        assert_eq!(unique_pci_match(adapters[0], &adapters, &devices), Some(1));
        assert_eq!(unique_pci_match(adapters[1], &adapters, &devices), Some(0));
        let mut wrong_subsystem = first;
        wrong_subsystem.pci_subsystem_id ^= 1;
        assert!(unique_pci_match(adapters[0], &adapters, &[wrong_subsystem]).is_none());
    }

    #[test]
    fn ambiguous_pci_domains_or_duplicate_windows_aliases_do_not_bind_sensors() {
        let first = pci_identity("0000:01:00.0");
        let another_domain = pci_identity("0001:01:00.0");
        let mut windows = first;
        windows.address.domain = None;
        assert!(unique_pci_match(windows, &[windows], &[first, another_domain]).is_none());
        assert!(unique_pci_match(windows, &[windows, windows], &[first]).is_none());
        assert!(unique_pci_match(windows, &[windows], &[]).is_none());
        // Missing a DLL/symbol, a blank output or an unterminated driver string
        // never turns into a fabricated driver version or sensor value.
        assert!(nvml_c_string(&[0; 8]).is_none());
        assert!(nvml_c_string(&[65; 8]).is_none());
        assert_eq!(nvml_c_string(&[65, 66, 0]), Some("AB".into()));
        assert!(GpuSample::default().temperature_celsius.is_none());
        assert!(GpuSample::default().power_watts.is_none());
    }

    fn value(name: &str, value: f64) -> CounterValue {
        CounterValue {
            name: name.into(),
            value,
        }
    }

    fn adapter_capacities() -> HashMap<u64, u64> {
        [(0xA, 16_384), (0xB, 16_384), (0xC, 0)]
            .into_iter()
            .collect()
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
        let aggregate = aggregate_counters(&snapshot, &adapter_capacities());
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
        let aggregate = aggregate_counters(&snapshot, &adapter_capacities());
        assert_eq!(aggregate.dedicated[&0xA], 4096);
        assert_eq!(aggregate.process_memory[&1], 4096);
        assert_eq!(aggregate.process_memory[&2], 3072);
        assert!(!aggregate.shared.contains_key(&0xA));
    }

    #[test]
    fn missing_and_invalid_counters_do_not_become_zero_usage() {
        let aggregate = aggregate_counters(&CounterSnapshot::default(), &adapter_capacities());
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
        let aggregate = aggregate_counters(&snapshot, &adapter_capacities());
        assert!(!aggregate.dedicated.contains_key(&0xA));
        assert!(!aggregate.dedicated.contains_key(&0xB));
        assert_eq!(aggregate.dedicated.get(&0xC), Some(&0));
    }

    #[test]
    fn indirect_display_aliases_do_not_pollute_physical_gpu_or_pid_totals() {
        let snapshot = CounterSnapshot {
            engines: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 25.0),
                value("pid_1_luid_0x0_0xFFFF_phys_0_eng_0_engtype_3D", 99.0),
                value("pid_2_luid_0x0_0xFFFF_phys_0_eng_0_engtype_3D", 50.0),
            ]),
            adapter_dedicated: Some(vec![
                value("luid_0x0_0xA_phys_0", 4096.0),
                value("luid_0x0_0xFFFF_phys_0", 65536.0),
            ]),
            adapter_shared: Some(vec![value("luid_0x0_0xFFFF_phys_0", 65536.0)]),
            process_dedicated: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0", 1024.0),
                value("pid_1_luid_0x0_0xFFFF_phys_0", 65536.0),
                value("pid_2_luid_0x0_0xFFFF_phys_0", 65536.0),
            ]),
            process_shared: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0", 512.0),
                value("pid_1_luid_0x0_0xFFFF_phys_0", 65536.0),
                value("pid_2_luid_0x0_0xFFFF_phys_0", 65536.0),
            ]),
            ..Default::default()
        };
        let aggregate = aggregate_counters(&snapshot, &adapter_capacities());
        assert_eq!(aggregate.process_usage[&1], 25.0);
        assert_eq!(aggregate.process_memory[&1], 1536);
        assert!(!aggregate.process_usage.contains_key(&2));
        assert!(!aggregate.process_memory.contains_key(&2));
        assert!(!aggregate.dedicated.contains_key(&0xFFFF));
        assert!(!aggregate.shared.contains_key(&0xFFFF));
        assert_eq!(aggregate.engines.len(), 1);
        assert!(aggregate.invalid_process_memory_pids.is_empty());
    }

    #[test]
    fn impossible_process_memory_is_unavailable_without_corrupting_adapter_totals() {
        let snapshot = CounterSnapshot {
            engines: Some(vec![value(
                "pid_1_luid_0x0_0xA_phys_0_eng_0_engtype_3D",
                10.0,
            )]),
            adapter_dedicated: Some(vec![value("luid_0x0_0xA_phys_0", 4096.0)]),
            process_dedicated: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0", 65536.0),
                value("pid_1_luid_0x0_0xB_phys_0", 128.0),
                value("pid_2_luid_0x0_0xA_phys_0", 1024.0),
                value("pid_3_luid_0x0_0xC_phys_0", 1024.0),
            ]),
            process_shared: Some(vec![
                value("pid_1_luid_0x0_0xA_phys_0", 0.0),
                value("pid_1_luid_0x0_0xB_phys_0", 512.0),
                value("pid_2_luid_0x0_0xA_phys_0", 512.0),
                value("pid_3_luid_0x0_0xC_phys_0", 0.0),
            ]),
            ..Default::default()
        };
        let aggregate = aggregate_counters(&snapshot, &adapter_capacities());
        assert!(!aggregate.process_memory.contains_key(&1));
        assert!(aggregate.invalid_process_memory_pids.contains(&1));
        assert_eq!(aggregate.process_memory[&2], 1536);
        // An unknown/zero capacity cannot establish that a value is impossible.
        assert_eq!(aggregate.process_memory[&3], 1024);
        assert_eq!(aggregate.dedicated[&0xA], 4096);
        assert_eq!(aggregate.process_usage[&1], 10.0);
    }
}

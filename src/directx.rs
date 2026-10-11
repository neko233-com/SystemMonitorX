//! One-time, adapter-specific DirectX 12 capability inventory.
//!
//! Collect this on a blocking worker, then cache the plain Rust result. This does
//! not change GPUI's renderer. COM objects and kernel adapter handles never escape
//! the collection call, and adapter identity uses the DXGI LUID rather than names.

use std::{collections::HashSet, ffi::c_void, mem::size_of, time::Instant};

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, LUID},
        Graphics::{
            Direct3D::{
                D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_12_1, D3D_FEATURE_LEVEL_12_2,
            },
            Direct3D12::{
                D3D_SHADER_MODEL, D3D_SHADER_MODEL_5_1, D3D_SHADER_MODEL_6_0, D3D_SHADER_MODEL_6_1,
                D3D_SHADER_MODEL_6_2, D3D_SHADER_MODEL_6_3, D3D_SHADER_MODEL_6_4,
                D3D_SHADER_MODEL_6_5, D3D_SHADER_MODEL_6_6, D3D_SHADER_MODEL_6_7,
                D3D_SHADER_MODEL_6_8, D3D_SHADER_MODEL_6_9, D3D12_FEATURE,
                D3D12_FEATURE_D3D12_OPTIONS5, D3D12_FEATURE_D3D12_OPTIONS7,
                D3D12_FEATURE_D3D12_OPTIONS12, D3D12_FEATURE_DATA_D3D12_OPTIONS5,
                D3D12_FEATURE_DATA_D3D12_OPTIONS7, D3D12_FEATURE_DATA_D3D12_OPTIONS12,
                D3D12_FEATURE_DATA_FEATURE_LEVELS, D3D12_FEATURE_DATA_SHADER_MODEL,
                D3D12_FEATURE_FEATURE_LEVELS, D3D12_FEATURE_SHADER_MODEL, D3D12CreateDevice,
                ID3D12Device,
            },
            Dxgi::{
                CreateDXGIFactory1, DXGI_ADAPTER_FLAG_REMOTE, DXGI_ADAPTER_FLAG_SOFTWARE,
                DXGI_ERROR_NOT_FOUND, DXGI_ERROR_UNSUPPORTED, IDXGIAdapter1, IDXGIFactory1,
            },
        },
    },
    core::{Error as WindowsError, Result as WindowsResult},
};

#[derive(Clone, Debug, Default)]
pub struct DirectXInventory {
    pub adapters: Vec<AdapterCapabilities>,
    /// Factory/enumeration issues. Per-adapter issues live on the corresponding entry.
    pub issues: Vec<String>,
    pub collection_duration_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct AdapterCapabilities {
    pub name: String,
    /// The same unsigned LUID representation used by GpuSample.luid.
    pub luid: u64,
    pub vendor_id: u32,
    pub device_id: u32,
    pub subsystem_id: u32,
    pub revision: u32,
    /// "hardware", "software", "indirect-display", "paravirtualized", or "unknown".
    pub classification: String,
    pub is_software: bool,
    /// The DXGI descriptor flag only; Microsoft currently documents it as reserved.
    pub is_remote: bool,
    /// None when the kernel adapter type could not be queried.
    pub is_virtual: Option<bool>,
    pub adapter_type_flags: Option<u32>,
    /// Some(false) only for DXGI_ERROR_UNSUPPORTED, never for an arbitrary failure.
    pub d3d12_supported: Option<bool>,
    /// Feature level available through this system's current runtime and driver.
    pub feature_level: Option<String>,
    pub shader_model: Option<String>,
    pub raytracing_tier: Option<String>,
    pub mesh_shader_tier: Option<String>,
    pub sampler_feedback_tier: Option<String>,
    pub enhanced_barriers: Option<bool>,
    pub node_count: Option<u32>,
    /// Explicit reasons for unavailable, unsupported, skipped, or failed fields.
    pub issues: Vec<String>,
}

impl DirectXInventory {
    /// Performs one read-only hardware inventory. Run on a blocking worker.
    ///
    /// A factory failure returns Err; a later adapter or capability failure leaves
    /// the other entries usable and records a partial issue instead.
    pub fn collect() -> Result<Self> {
        let started = Instant::now();
        // SAFETY: this creates the requested COM factory and transfers its owned
        // reference into the windows wrapper. DXGI/D3D12 do not require COM init.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.context("创建 DXGI 工厂失败")?;
        let mut inventory = Self::default();
        let mut seen = HashSet::new();
        let mut index = 0_u32;
        loop {
            // SAFETY: factory is live; enumeration returns an owned adapter.
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => {
                    inventory.issues.push(format!(
                        "枚举 DXGI 适配器 {index} 失败：{}",
                        describe_error(&error)
                    ));
                    break;
                }
            };
            // SAFETY: adapter is live and GetDesc1 writes into its own result storage.
            match unsafe { adapter.GetDesc1() } {
                Ok(desc) => {
                    let luid = luid_value(desc.AdapterLuid);
                    if seen.insert(luid) {
                        let name_end = desc
                            .Description
                            .iter()
                            .position(|&value| value == 0)
                            .unwrap_or(desc.Description.len());
                        let mut entry = AdapterCapabilities {
                            name: String::from_utf16_lossy(&desc.Description[..name_end]),
                            luid,
                            vendor_id: desc.VendorId,
                            device_id: desc.DeviceId,
                            subsystem_id: desc.SubSysId,
                            revision: desc.Revision,
                            is_software: desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
                            is_remote: desc.Flags & DXGI_ADAPTER_FLAG_REMOTE.0 as u32 != 0,
                            ..Default::default()
                        };
                        match query_adapter_type(desc.AdapterLuid) {
                            Ok(flags) => entry.adapter_type_flags = Some(flags),
                            Err(issue) => entry.issues.push(issue),
                        }
                        let (classification, is_software, is_virtual) =
                            classify_adapter(entry.is_software, entry.adapter_type_flags);
                        entry.classification = classification.to_owned();
                        entry.is_software = is_software;
                        entry.is_virtual = is_virtual;
                        if entry.is_software || entry.is_virtual == Some(true) || entry.is_remote {
                            entry.issues.push(
                                "软件、虚拟或远程适配器：已跳过物理 GPU 的 D3D12 能力查询。".into(),
                            );
                        } else {
                            collect_capabilities(&adapter, &mut entry);
                        }
                        inventory.adapters.push(entry);
                    } else {
                        inventory.issues.push(format!(
                            "DXGI 返回重复适配器 LUID {luid:016X}，已保留首次枚举的条目。"
                        ));
                    }
                }
                Err(error) => inventory.issues.push(format!(
                    "读取 DXGI 适配器 {index} 描述失败：{}",
                    describe_error(&error)
                )),
            }
            let Some(next) = index.checked_add(1) else {
                inventory.issues.push("DXGI 适配器索引超出范围。".into());
                break;
            };
            index = next;
        }
        inventory.collection_duration_ms =
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(inventory)
    }
}

fn collect_capabilities(adapter: &IDXGIAdapter1, entry: &mut AdapterCapabilities) {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: adapter is live and device is valid storage for an owned COM
    // reference. Creating a device really verifies this adapter's D3D12 support:
    // https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-d3d12createdevice
    if let Err(error) = unsafe { D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_11_0, &mut device) } {
        if error.code() == DXGI_ERROR_UNSUPPORTED {
            entry.d3d12_supported = Some(false);
        }
        entry
            .issues
            .push(format!("创建 D3D12 设备失败：{}", describe_error(&error)));
        return;
    }
    let Some(device) = device else {
        entry
            .issues
            .push("D3D12CreateDevice 成功但没有返回设备，能力状态未知。".into());
        return;
    };
    // SAFETY: the device is live. Identity is the LUID, not a COM pointer or name.
    if luid_value(unsafe { device.GetAdapterLuid() }) != entry.luid {
        entry
            .issues
            .push("D3D12 设备 LUID 与枚举的适配器不一致，已拒绝关联能力。".into());
        return;
    }
    entry.d3d12_supported = Some(true);
    // SAFETY: the live device returns its hardware node count without arguments.
    let node_count = unsafe { device.GetNodeCount() };
    if node_count > 0 {
        entry.node_count = Some(node_count);
    } else {
        entry
            .issues
            .push("D3D12 设备返回零节点，节点数未知。".into());
    }

    match query_feature_level(&device) {
        Ok(value) => match feature_level_name(value) {
            Some(name) => entry.feature_level = Some(name.into()),
            None => entry
                .issues
                .push(format!("D3D12 返回未知 Feature Level 0x{:X}。", value.0)),
        },
        Err(error) => entry.issues.push(feature_issue("Feature Level", &error)),
    }
    match query_shader_model(&device) {
        Ok(value) => match shader_model_name(value) {
            Some(name) => entry.shader_model = Some(name.into()),
            None => entry
                .issues
                .push(format!("D3D12 返回未知 Shader Model 0x{:X}。", value.0)),
        },
        Err(error) => entry.issues.push(feature_issue("Shader Model", &error)),
    }

    let mut options5 = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    // SAFETY: OPTIONS5 exactly matches the requested feature's documented ABI.
    match unsafe { query_feature(&device, D3D12_FEATURE_D3D12_OPTIONS5, &mut options5) } {
        Ok(()) => {
            entry.raytracing_tier = record_tier(
                "光线追踪",
                options5.RaytracingTier.0,
                raytracing_name,
                &mut entry.issues,
            );
        }
        Err(error) => entry
            .issues
            .push(feature_issue("光线追踪 / Options5", &error)),
    }

    let mut options7 = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
    // SAFETY: OPTIONS7 exactly matches the requested feature's documented ABI.
    match unsafe { query_feature(&device, D3D12_FEATURE_D3D12_OPTIONS7, &mut options7) } {
        Ok(()) => {
            entry.mesh_shader_tier = record_tier(
                "Mesh Shader",
                options7.MeshShaderTier.0,
                mesh_shader_name,
                &mut entry.issues,
            );
            entry.sampler_feedback_tier = record_tier(
                "Sampler Feedback",
                options7.SamplerFeedbackTier.0,
                sampler_feedback_name,
                &mut entry.issues,
            );
        }
        Err(error) => entry.issues.push(feature_issue(
            "Mesh Shader / Sampler Feedback / Options7",
            &error,
        )),
    }

    let mut options12 = D3D12_FEATURE_DATA_D3D12_OPTIONS12::default();
    // SAFETY: OPTIONS12 exactly matches the requested feature's documented ABI.
    // Old system runtimes may reject this query. Never infer false from failure.
    // https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_feature_data_d3d12_options12
    match unsafe { query_feature(&device, D3D12_FEATURE_D3D12_OPTIONS12, &mut options12) } {
        Ok(()) => entry.enhanced_barriers = Some(options12.EnhancedBarriersSupported.as_bool()),
        Err(error) => entry
            .issues
            .push(feature_issue("Enhanced Barriers / Options12", &error)),
    }
}

/// The runtime can reject feature-level/model values introduced by newer SDKs.
/// Only E_INVALIDARG permits dropping the newest request and trying the next;
/// device-removal/driver failures must remain failures, not a downgraded result.
fn retry_runtime_values<T: Copy, R>(
    values: &[T],
    mut query: impl FnMut(T) -> WindowsResult<R>,
) -> WindowsResult<R> {
    let mut last_error = WindowsError::from_hresult(E_INVALIDARG);
    for &value in values {
        match query(value) {
            Ok(result) => return Ok(result),
            Err(error) if error.code() == E_INVALIDARG => last_error = error,
            Err(error) => return Err(error),
        }
    }
    Err(last_error)
}

fn query_feature_level(device: &ID3D12Device) -> WindowsResult<D3D_FEATURE_LEVEL> {
    let levels = [
        D3D_FEATURE_LEVEL_12_2,
        D3D_FEATURE_LEVEL_12_1,
        D3D_FEATURE_LEVEL_12_0,
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
    ];
    retry_runtime_values(&[0_usize, 1, 2, 3, 4], |offset| {
        let requested = &levels[offset..];
        let mut data = D3D12_FEATURE_DATA_FEATURE_LEVELS {
            NumFeatureLevels: requested.len() as u32,
            pFeatureLevelsRequested: requested.as_ptr(),
            ..Default::default()
        };
        // SAFETY: the requested feature-level slice remains live for this call,
        // and data has precisely the FEATURE_LEVELS layout and size.
        unsafe { query_feature(device, D3D12_FEATURE_FEATURE_LEVELS, &mut data) }?;
        Ok(data.MaxSupportedFeatureLevel)
    })
}

fn query_shader_model(device: &ID3D12Device) -> WindowsResult<D3D_SHADER_MODEL> {
    // Highest model represented by the pinned Windows SDK bindings, descending.
    // https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_feature_data_shader_model
    let models = [
        D3D_SHADER_MODEL_6_9,
        D3D_SHADER_MODEL_6_8,
        D3D_SHADER_MODEL_6_7,
        D3D_SHADER_MODEL_6_6,
        D3D_SHADER_MODEL_6_5,
        D3D_SHADER_MODEL_6_4,
        D3D_SHADER_MODEL_6_3,
        D3D_SHADER_MODEL_6_2,
        D3D_SHADER_MODEL_6_1,
        D3D_SHADER_MODEL_6_0,
        D3D_SHADER_MODEL_5_1,
    ];
    retry_runtime_values(&models, |model| {
        let mut data = D3D12_FEATURE_DATA_SHADER_MODEL {
            HighestShaderModel: model,
        };
        // SAFETY: data matches SHADER_MODEL and is initialized to the runtime
        // ceiling being tested; an unsupported ceiling yields E_INVALIDARG.
        unsafe { query_feature(device, D3D12_FEATURE_SHADER_MODEL, &mut data) }?;
        Ok(data.HighestShaderModel)
    })
}

/// data must have the exact layout required by feature.
unsafe fn query_feature<T>(
    device: &ID3D12Device,
    feature: D3D12_FEATURE,
    data: &mut T,
) -> WindowsResult<()> {
    // SAFETY: callers provide the specific Windows feature struct matching the
    // feature enum. The mutable borrow remains live for the synchronous call.
    unsafe { device.CheckFeatureSupport(feature, (data as *mut T).cast(), size_of::<T>() as u32) }
}

fn feature_level_name(value: D3D_FEATURE_LEVEL) -> Option<&'static str> {
    match value {
        D3D_FEATURE_LEVEL_11_0 => Some("11_0"),
        D3D_FEATURE_LEVEL_11_1 => Some("11_1"),
        D3D_FEATURE_LEVEL_12_0 => Some("12_0"),
        D3D_FEATURE_LEVEL_12_1 => Some("12_1"),
        D3D_FEATURE_LEVEL_12_2 => Some("12_2"),
        _ => None,
    }
}

fn shader_model_name(value: D3D_SHADER_MODEL) -> Option<&'static str> {
    match value {
        D3D_SHADER_MODEL_5_1 => Some("5.1"),
        D3D_SHADER_MODEL_6_0 => Some("6.0"),
        D3D_SHADER_MODEL_6_1 => Some("6.1"),
        D3D_SHADER_MODEL_6_2 => Some("6.2"),
        D3D_SHADER_MODEL_6_3 => Some("6.3"),
        D3D_SHADER_MODEL_6_4 => Some("6.4"),
        D3D_SHADER_MODEL_6_5 => Some("6.5"),
        D3D_SHADER_MODEL_6_6 => Some("6.6"),
        D3D_SHADER_MODEL_6_7 => Some("6.7"),
        D3D_SHADER_MODEL_6_8 => Some("6.8"),
        D3D_SHADER_MODEL_6_9 => Some("6.9"),
        _ => None,
    }
}

fn raytracing_name(value: i32) -> Option<&'static str> {
    match value {
        10 => Some("1.0"),
        11 => Some("1.1"),
        // Tier 1.2 is documented by Microsoft but newer than the pinned bindings:
        // https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ne-d3d12-d3d12_raytracing_tier
        12 => Some("1.2"),
        _ => None,
    }
}

fn mesh_shader_name(value: i32) -> Option<&'static str> {
    (value == 10).then_some("1")
}

fn sampler_feedback_name(value: i32) -> Option<&'static str> {
    match value {
        90 => Some("0.9"),
        100 => Some("1.0"),
        _ => None,
    }
}

fn record_tier(
    label: &str,
    value: i32,
    mapping: impl FnOnce(i32) -> Option<&'static str>,
    issues: &mut Vec<String>,
) -> Option<String> {
    if let Some(name) = mapping(value) {
        Some(name.into())
    } else {
        if value == 0 {
            issues.push(format!("{label}：设备明确报告不支持该能力。"));
        } else {
            issues.push(format!("{label}：设备返回未知能力等级 {value}。"));
        }
        None
    }
}

fn feature_issue(label: &str, error: &WindowsError) -> String {
    if error.code() == E_INVALIDARG {
        format!(
            "{label} 查询不可用（当前运行时不识别查询或请求）：{}",
            describe_error(error)
        )
    } else {
        format!("{label} 查询失败：{}", describe_error(error))
    }
}

fn describe_error(error: &WindowsError) -> String {
    format!("HRESULT 0x{:08X} ({error})", error.code().0 as u32)
}

fn luid_value(luid: LUID) -> u64 {
    ((luid.HighPart as u32 as u64) << 32) | luid.LowPart as u64
}

fn classify_adapter(
    dxgi_software: bool,
    adapter_type: Option<u32>,
) -> (&'static str, bool, Option<bool>) {
    let software = dxgi_software || adapter_type.is_some_and(|flags| flags & (1 << 2) != 0);
    let virtual_adapter = adapter_type.map(|flags| flags & ((1 << 6) | (1 << 7)) != 0);
    if software {
        ("software", true, virtual_adapter)
    } else if adapter_type.is_some_and(|flags| flags & (1 << 6) != 0) {
        ("indirect-display", false, Some(true))
    } else if adapter_type.is_some_and(|flags| flags & (1 << 7) != 0) {
        ("paravirtualized", false, Some(true))
    } else if adapter_type.is_some() {
        ("hardware", false, Some(false))
    } else {
        ("unknown", false, None)
    }
}

#[repr(C)]
struct OpenAdapter {
    luid: LUID,
    handle: u32,
}

#[repr(C)]
struct QueryAdapter {
    handle: u32,
    kind: i32,
    data: *mut c_void,
    size: u32,
}

#[repr(C)]
struct AdapterHandle {
    handle: u32,
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTOpenAdapterFromLuid(adapter: *mut OpenAdapter) -> i32;
    fn D3DKMTQueryAdapterInfo(query: *const QueryAdapter) -> i32;
    fn D3DKMTCloseAdapter(adapter: *const AdapterHandle) -> i32;
}

impl Drop for AdapterHandle {
    fn drop(&mut self) {
        // SAFETY: this owns a successfully opened kernel adapter handle and
        // releases it exactly once, also on all query-error paths.
        unsafe { D3DKMTCloseAdapter(self) };
    }
}

fn query_adapter_type(luid: LUID) -> std::result::Result<u32, String> {
    let mut open = OpenAdapter { luid, handle: 0 };
    // SAFETY: open is initialized in/out storage with the documented ABI.
    let status = unsafe { D3DKMTOpenAdapterFromLuid(&mut open) };
    if status < 0 {
        return Err(format!(
            "读取内核适配器类型失败：D3DKMTOpenAdapterFromLuid NTSTATUS 0x{:08X}。",
            status as u32
        ));
    }
    let handle = AdapterHandle {
        handle: open.handle,
    };
    if handle.handle == 0 {
        return Err("读取内核适配器类型失败：内核返回空适配器句柄。".into());
    }
    let mut flags = 0_u32;
    let query = QueryAdapter {
        handle: handle.handle,
        kind: 15, // KMTQAITYPE_ADAPTERTYPE
        data: (&mut flags as *mut u32).cast(),
        size: size_of::<u32>() as u32,
    };
    // SAFETY: handle is live and data is a writable 32-bit D3DKMT_ADAPTERTYPE.
    // IndirectDisplayDevice=bit6, Paravirtualized=bit7, SoftwareDevice=bit2:
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_adaptertype
    let status = unsafe { D3DKMTQueryAdapterInfo(&query) };
    if status < 0 {
        Err(format!(
            "读取内核适配器类型失败：D3DKMTQueryAdapterInfo NTSTATUS 0x{:08X}。",
            status as u32
        ))
    } else {
        Ok(flags)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_adapter, feature_level_name, luid_value, mesh_shader_name, raytracing_name,
        record_tier, retry_runtime_values, sampler_feedback_name, shader_model_name,
    };
    use windows::{
        Win32::{
            Foundation::{E_INVALIDARG, E_OUTOFMEMORY, LUID},
            Graphics::{
                Direct3D::{D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_12_2},
                Direct3D12::{D3D_SHADER_MODEL, D3D_SHADER_MODEL_5_1, D3D_SHADER_MODEL_6_9},
            },
        },
        core::Error,
    };

    #[test]
    fn known_sdk_values_map_without_inventing_unknown_capabilities() {
        assert_eq!(feature_level_name(D3D_FEATURE_LEVEL_11_0), Some("11_0"));
        assert_eq!(feature_level_name(D3D_FEATURE_LEVEL_12_2), Some("12_2"));
        assert_eq!(feature_level_name(D3D_FEATURE_LEVEL(0)), None);
        assert_eq!(shader_model_name(D3D_SHADER_MODEL_5_1), Some("5.1"));
        assert_eq!(shader_model_name(D3D_SHADER_MODEL_6_9), Some("6.9"));
        assert_eq!(shader_model_name(D3D_SHADER_MODEL(106)), None);
        assert_eq!(raytracing_name(12), Some("1.2"));
        assert_eq!(mesh_shader_name(10), Some("1"));
        assert_eq!(sampler_feedback_name(90), Some("0.9"));
        assert_eq!(sampler_feedback_name(100), Some("1.0"));
    }

    #[test]
    fn unsupported_and_future_tiers_are_missing_with_explicit_reasons() {
        let mut issues = Vec::new();
        assert_eq!(
            record_tier("Ray tracing", 0, raytracing_name, &mut issues),
            None
        );
        assert_eq!(record_tier("Mesh", 99, mesh_shader_name, &mut issues), None);
        assert_eq!(
            record_tier("Feedback", 100, sampler_feedback_name, &mut issues),
            Some("1.0".into())
        );
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("不支持"));
        assert!(issues[1].contains("99"));
    }

    #[test]
    fn old_runtime_retries_invalid_argument_in_descending_order() {
        let mut requests = Vec::new();
        let result = retry_runtime_values(&[105, 104, 103], |model| {
            requests.push(model);
            if model > 103 {
                Err(Error::from_hresult(E_INVALIDARG))
            } else {
                Ok(101)
            }
        })
        .unwrap();
        assert_eq!(requests, vec![105, 104, 103]);
        assert_eq!(result, 101);
    }

    #[test]
    fn actual_query_failure_does_not_silently_downgrade() {
        let mut requests = Vec::new();
        let error = retry_runtime_values(&[105, 104, 103], |model| {
            requests.push(model);
            Err::<i32, _>(Error::from_hresult(E_OUTOFMEMORY))
        })
        .unwrap_err();
        assert_eq!(error.code(), E_OUTOFMEMORY);
        assert_eq!(requests, vec![105]);
        assert!(retry_runtime_values::<i32, i32>(&[], |_| Ok(0)).is_err());
        assert!(
            retry_runtime_values::<i32, i32>(&[105], |_| Err(Error::from_hresult(E_INVALIDARG)))
                .is_err()
        );
    }

    #[test]
    fn kernel_flags_distinguish_virtual_software_and_unknown_adapters() {
        assert_eq!(
            classify_adapter(false, Some(3)),
            ("hardware", false, Some(false))
        );
        assert_eq!(
            classify_adapter(false, Some(1 << 6)),
            ("indirect-display", false, Some(true))
        );
        assert_eq!(
            classify_adapter(false, Some(1 << 7)),
            ("paravirtualized", false, Some(true))
        );
        assert_eq!(classify_adapter(true, None), ("software", true, None));
        assert_eq!(
            classify_adapter(false, Some(1 << 2)),
            ("software", true, Some(false))
        );
        assert_eq!(classify_adapter(false, None), ("unknown", false, None));
    }

    #[test]
    fn luid_preserves_signed_high_part_bits_and_low_part() {
        assert_eq!(
            luid_value(LUID {
                HighPart: -1,
                LowPart: 0x1234_5678
            }),
            0xFFFF_FFFF_1234_5678
        );
    }
}

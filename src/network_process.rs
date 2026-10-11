//! Per-process TCP/UDP IPv4/IPv6 traffic from Windows' Kernel-Network ETW
//! provider. This is packet-byte telemetry, including loopback; it is not
//! NIC throughput apportioned to processes and not TCP connection estimates.
//!
//! The payload PID is authoritative (the event-header PID can be a worker).
//! TDH discovers each event version's field names, types and fixed offsets.
//! Variable-layout fields use TdhGetProperty instead. Unknown schemas, dropped
//! events and insufficient privileges produce unavailable samples, never zero.
//! See Microsoft Learn's TcpIp_TypeGroup1, UdpIp_TypeGroup1, TdhGetEventInformation
//! and Consuming Events documentation. Provider GUID/task/opcodes are confirmed
//! by the Windows provider's installed, compiled manifest metadata.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, GetLastError};
use windows::Win32::System::Diagnostics::Etw::*;
use windows::core::{GUID, PCWSTR, PWSTR};

const KERNEL_NETWORK_PROVIDER: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
const NETWORK_KEYWORDS: u64 = 0x10 | 0x20; // Installed IPv4 and IPv6 keywords.
const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_SCHEMA_COUNT: usize = 128;

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessNetworkRate {
    /// Exact process creation FILETIME; the PID alone is not an identity.
    pub process_start_time_windows_ticks: u64,
    pub received_bytes_per_sec: f64,
    pub sent_bytes_per_sec: f64,
    /// Actual bytes delivered during this sample interval, before division.
    pub received_bytes: u64,
    pub sent_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkProtocolCounts {
    pub tcp_ipv4: u64,
    pub tcp_ipv6: u64,
    pub udp_ipv4: u64,
    pub udp_ipv6: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessNetworkSnapshot {
    /// A missing PID means unavailable, new identity, or not tracked. When a
    /// stable tracked PID had no events in a complete interval its rate is zero.
    pub rates: HashMap<u32, ProcessNetworkRate>,
    pub available: bool,
    pub status: String,
    pub elapsed_seconds: f64,
    /// True only for an independently complete, decoded, no-loss interval.
    /// Availability alone may be true while the initial baseline is warming.
    pub complete_interval: bool,
    /// Monotonic interval identity; consumers can reject duplicates/replays.
    pub interval_end: Option<Instant>,
    pub dropped_events: u64,
    /// Lifetime successfully decoded events, for diagnostics and real probes.
    pub protocol_events: NetworkProtocolCounts,
}

pub struct ProcessNetworkMonitor {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    last_sample: Instant,
    last_loss_query: Instant,
    baseline_ready: bool,
}

impl Default for ProcessNetworkMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessNetworkMonitor {
    /// Returns immediately. StartTrace/OpenTrace/ProcessTrace run on a dedicated
    /// native thread. This does not request UAC or change token privileges.
    pub fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("SystemMonitorX.NetworkETW".into())
            .spawn(move || trace_worker(worker_shared));
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                shared.data().phase =
                    TracePhase::Unavailable(format!("无法创建逐进程网络采集线程：{error}"));
                None
            }
        };
        Self {
            shared,
            worker,
            last_sample: Instant::now(),
            last_loss_query: Instant::now(),
            baseline_ready: false,
        }
    }

    /// Each identity is (PID, exact Windows creation FILETIME in 100ns ticks).
    /// Do not pass Unix seconds. PID reuse drops the entire first interval of
    /// the new identity; delayed events older than its creation are rejected.
    /// Invoke on the monitor's collection thread, not the UI rendering thread.
    pub fn sample(&mut self, identities: &[(u32, u64)]) -> ProcessNetworkSnapshot {
        // Provider/consumer loss is independently queried, at most once/sec.
        // It does not require privilege adjustment and targets our handle only.
        if self.last_loss_query.elapsed() >= Duration::from_secs(1) {
            self.shared.query_losses();
            self.last_loss_query = Instant::now();
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_sample).as_secs_f64();
        self.last_sample = now;
        let mut state = self.shared.data();
        let phase_ready = matches!(state.phase, TracePhase::Running);
        let interval_complete = phase_ready
            && state.schema_failure.is_none()
            && !state.incomplete_interval
            && self.baseline_ready
            && elapsed > 0.0;
        let status = state.status(self.baseline_ready);
        let rates = state
            .tallies
            .finish_interval(identities, elapsed, interval_complete);
        let snapshot = ProcessNetworkSnapshot {
            rates,
            available: phase_ready && state.schema_failure.is_none() && !state.incomplete_interval,
            status,
            elapsed_seconds: elapsed,
            complete_interval: interval_complete,
            interval_end: Some(now),
            dropped_events: state.events_lost,
            protocol_events: state.protocol_events,
        };
        // A loss interval cannot be used as the next baseline, either. Resume
        // only after one complete, independently observed interval.
        self.baseline_ready =
            phase_ready && state.schema_failure.is_none() && !state.incomplete_interval;
        state.incomplete_interval = false;
        state.interval_error = None;
        snapshot
    }
}

impl Drop for ProcessNetworkMonitor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // CloseTrace aborts ProcessTrace and ControlTrace stops our own session.
        // Handles are taken exactly once under a mutex, including startup races.
        self.shared.stop_owned_trace();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<CollectorState>,
    handles: Mutex<TraceHandles>,
    stop: AtomicBool,
}

impl Shared {
    fn data(&self) -> MutexGuard<'_, CollectorState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn handles(&self) -> MutexGuard<'_, TraceHandles> {
        self.handles
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn stop_owned_trace(&self) {
        let mut owned = self.handles();
        if let Some(consumer) = owned.consumer.take() {
            // SAFETY: OpenTrace acquired this handle and take grants exclusive
            // closing ownership. Callback context lives until worker join.
            let _ = unsafe { CloseTrace(PROCESSTRACE_HANDLE { Value: consumer }) };
        }
        if let Some(controller) = owned.controller.take() {
            let mut properties = SessionProperties::control();
            // SAFETY: this is exactly the StartTrace handle owned by this
            // monitor, never a kernel/global session or another tool's name.
            let _ = unsafe {
                ControlTraceW(
                    CONTROLTRACE_HANDLE { Value: controller },
                    PCWSTR::null(),
                    &mut properties.properties,
                    EVENT_TRACE_CONTROL_STOP,
                )
            };
        }
    }

    fn query_losses(&self) {
        let owned = self.handles();
        let Some(controller) = owned.controller else {
            return;
        };
        let mut properties = SessionProperties::control();
        // SAFETY: handle is held alive under its ownership mutex and buffer is
        // valid. QUERY reads statistics without changing the provider/session.
        let result = unsafe {
            ControlTraceW(
                CONTROLTRACE_HANDLE { Value: controller },
                PCWSTR::null(),
                &mut properties.properties,
                EVENT_TRACE_CONTROL_QUERY,
            )
        };
        if result.0 == 0 {
            let mut state = self.data();
            state.observe_losses(
                u64::from(properties.properties.EventsLost),
                u64::from(properties.properties.RealTimeBuffersLost),
            );
        }
    }
}

#[derive(Default)]
struct TraceHandles {
    controller: Option<u64>,
    consumer: Option<u64>,
}

struct OwnedTraceGuard(Arc<Shared>);

impl Drop for OwnedTraceGuard {
    fn drop(&mut self) {
        self.0.stop_owned_trace();
    }
}

#[repr(C)]
struct SessionProperties {
    properties: EVENT_TRACE_PROPERTIES,
    name: [u16; 128],
}

impl SessionProperties {
    fn control() -> Self {
        let mut result = Self {
            properties: EVENT_TRACE_PROPERTIES::default(),
            name: [0; 128],
        };
        result.properties.Wnode.BufferSize = size_of::<Self>() as u32;
        result.properties.LoggerNameOffset = std::mem::offset_of!(Self, name) as u32;
        result
    }

    fn start(guid: GUID) -> Self {
        let mut result = Self::control();
        result.properties.Wnode.Guid = guid;
        result.properties.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // System time (100ns FILETIME), comparable with exact process creation
        // times. Default QPC timestamps must not be compared with FILETIME.
        result.properties.Wnode.ClientContext = 2;
        result.properties.BufferSize = 64;
        result.properties.MinimumBuffers = 4;
        result.properties.MaximumBuffers = 64;
        result.properties.LogFileMode =
            EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_NO_PER_PROCESSOR_BUFFERING;
        result.properties.FlushTimer = 1; // ETW's documented minimum, in seconds.
        result
    }
}

fn trace_worker(shared: Arc<Shared>) {
    let _guard = OwnedTraceGuard(Arc::clone(&shared));
    let guid = match GUID::new() {
        Ok(guid) => guid,
        Err(error) => {
            shared.data().phase =
                TracePhase::Unavailable(format!("无法创建独立网络追踪标识：{error}"));
            return;
        }
    };
    // A descriptive, per-instance private name. Never attaches to or stops an
    // existing session if another component happens to own a matching name.
    let name = format!("SystemMonitorX.Network.{}.{guid:?}", std::process::id());
    let mut wide_name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut properties = SessionProperties::start(guid);
    let mut controller = CONTROLTRACE_HANDLE::default();
    // SAFETY: contiguous properties/name storage with correct offsets and size.
    // The name is NUL-terminated and lives through OpenTrace/ProcessTrace.
    let result = unsafe {
        StartTraceW(
            &mut controller,
            PCWSTR(wide_name.as_ptr()),
            &mut properties.properties,
        )
    };
    if result.0 != 0 {
        shared.data().phase = TracePhase::Unavailable(trace_error("启动网络追踪", result.0));
        return;
    }
    shared.handles().controller = Some(controller.Value);
    if shared.stop.load(Ordering::Acquire) {
        return;
    }
    // Kernel-Network is a manifest provider in our own real-time session, so
    // there is no NT Kernel Logger takeover or system-wide audit configuration.
    // SAFETY: owned live session and static provider GUID, IPv4+IPv6 keywords.
    let result = unsafe {
        EnableTraceEx2(
            controller,
            &KERNEL_NETWORK_PROVIDER,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
            5,
            NETWORK_KEYWORDS,
            0,
            1000,
            None,
        )
    };
    if result.0 != 0 {
        shared.data().phase = TracePhase::Unavailable(trace_error("启用网络提供者", result.0));
        return;
    }
    let mut context = Box::new(CallbackContext {
        shared: Arc::clone(&shared),
        schemas: HashMap::new(),
    });
    let mut logfile = EVENT_TRACE_LOGFILEW {
        LoggerName: PWSTR(wide_name.as_mut_ptr()),
        Context: (&mut *context as *mut CallbackContext).cast(),
        BufferCallback: Some(buffer_callback),
        ..Default::default()
    };
    logfile.Anonymous1.ProcessTraceMode =
        PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(event_callback);
    // SAFETY: logger name/callback context remain allocated for entire worker.
    let consumer = unsafe { OpenTraceW(&mut logfile) };
    if consumer.Value == u64::MAX {
        // SAFETY: read immediately after OpenTrace's invalid-handle result.
        let error = unsafe { GetLastError() };
        shared.data().phase = TracePhase::Unavailable(trace_error("打开网络事件流", error.0));
        return;
    }
    shared.handles().consumer = Some(consumer.Value);
    if shared.stop.load(Ordering::Acquire) {
        return;
    }
    shared.data().phase = TracePhase::Running;
    // SAFETY: one owned real-time consumer; callbacks only run on this thread,
    // and all pointed-to context/logfile/name storage remains live until return.
    let result = unsafe { ProcessTrace(&[consumer], None, None) };
    if !shared.stop.load(Ordering::Acquire) {
        shared.data().phase = TracePhase::Unavailable(trace_error("网络事件流已停止", result.0));
    }
    // Guard closes consumer/stops controller before context is finally freed.
    shared.stop_owned_trace();
}

fn trace_error(action: &str, error: u32) -> String {
    if error == ERROR_ACCESS_DENIED.0 || error == 1314 {
        format!(
            "逐进程网络不可用：{action}被 Windows 拒绝（{error}）；需要管理员或已授予的 ETW 性能追踪权限"
        )
    } else {
        format!("逐进程网络不可用：{action}，Windows 错误 {error}")
    }
}

#[derive(Default)]
enum TracePhase {
    #[default]
    Starting,
    Running,
    Unavailable(String),
}

#[derive(Default)]
struct CollectorState {
    phase: TracePhase,
    tallies: Tallies,
    schema_failure: Option<String>,
    incomplete_interval: bool,
    interval_error: Option<String>,
    events_lost: u64,
    buffers_lost: u64,
    protocol_events: NetworkProtocolCounts,
}

impl CollectorState {
    fn status(&self, baseline_ready: bool) -> String {
        match &self.phase {
            TracePhase::Starting => "正在启动逐进程网络 ETW 采集".into(),
            TracePhase::Unavailable(error) => error.clone(),
            TracePhase::Running if self.schema_failure.is_some() => {
                format!("逐进程网络不可用：{}", self.schema_failure.as_deref().unwrap_or_default())
            }
            TracePhase::Running if self.incomplete_interval => self.interval_error.clone()
                .unwrap_or_else(|| "网络追踪丢失事件；本周期逐进程网络显示为不可用".into()),
            TracePhase::Running if !baseline_ready => "逐进程网络正在建立完整采样基线".into(),
            TracePhase::Running => "ETW · TCP/UDP · IPv4/IPv6 · 按真实采样时长统计包字节（含回环；事件刷新最长约 1 秒）".into(),
        }
    }

    fn observe_losses(&mut self, events: u64, buffers: u64) {
        if events > self.events_lost || buffers > self.buffers_lost {
            self.incomplete_interval = true;
        }
        self.events_lost = self.events_lost.max(events);
        self.buffers_lost = self.buffers_lost.max(buffers);
    }
}

#[derive(Default)]
struct Tallies {
    tracked: HashMap<u32, u64>,
    bytes: HashMap<u32, ByteCount>,
}

#[derive(Clone, Copy, Default)]
struct ByteCount {
    received: u64,
    sent: u64,
}

impl Tallies {
    fn record(&mut self, pid: u32, timestamp: u64, bytes: u64, direction: Direction) {
        let Some(creation) = self.tracked.get(&pid) else {
            return;
        };
        if *creation == 0 || timestamp < *creation {
            return;
        }
        let total = self.bytes.entry(pid).or_default();
        match direction {
            Direction::Receive => total.received = total.received.saturating_add(bytes),
            Direction::Send => total.sent = total.sent.saturating_add(bytes),
        }
    }

    fn finish_interval(
        &mut self,
        identities: &[(u32, u64)],
        elapsed: f64,
        complete: bool,
    ) -> HashMap<u32, ProcessNetworkRate> {
        let mut rates = HashMap::new();
        let next: HashMap<_, _> = identities
            .iter()
            .copied()
            .filter(|(_, birth)| *birth > 0)
            .collect();
        if complete && elapsed.is_finite() && elapsed > 0.0 {
            for (&pid, &birth) in &next {
                if self.tracked.get(&pid) != Some(&birth) {
                    continue;
                }
                let bytes = self.bytes.get(&pid).copied().unwrap_or_default();
                rates.insert(
                    pid,
                    ProcessNetworkRate {
                        process_start_time_windows_ticks: birth,
                        received_bytes_per_sec: bytes.received as f64 / elapsed,
                        sent_bytes_per_sec: bytes.sent as f64 / elapsed,
                        received_bytes: bytes.received,
                        sent_bytes: bytes.sent,
                    },
                );
            }
        }
        self.tracked = next;
        self.bytes.clear();
        rates
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Send,
    Receive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    TcpV4,
    TcpV6,
    UdpV4,
    UdpV6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SchemaKey {
    id: u16,
    version: u8,
    opcode: u8,
    task: u16,
    flags: u16,
}

struct CallbackContext {
    shared: Arc<Shared>,
    schemas: HashMap<SchemaKey, Result<NetworkSchema, String>>,
}

unsafe extern "system" fn event_callback(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }
    // SAFETY: ETW owns a valid callback record for this invocation.
    let record = unsafe { &*record };
    if record.UserContext.is_null() || record.EventHeader.ProviderId != KERNEL_NETWORK_PROVIDER {
        return;
    }
    // SAFETY: OpenTrace passed our live boxed context. A single ProcessTrace
    // call invokes these callbacks serially on its native consumer thread.
    let context = unsafe { &mut *record.UserContext.cast::<CallbackContext>() };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.event(record)));
    if result.is_err() {
        let mut state = context.shared.data();
        state.schema_failure = Some("网络事件解码发生异常，已停止提供不完整计数".into());
    }
}

unsafe extern "system" fn buffer_callback(logfile: *mut EVENT_TRACE_LOGFILEW) -> u32 {
    if logfile.is_null() {
        return 0;
    }
    // SAFETY: ETW passes a live logfile and the original callback context.
    let logfile = unsafe { &*logfile };
    if logfile.Context.is_null() {
        return 0;
    }
    let context = unsafe { &*logfile.Context.cast::<CallbackContext>() };
    context
        .shared
        .data()
        .observe_losses(u64::from(logfile.EventsLost), 0);
    u32::from(!context.shared.stop.load(Ordering::Acquire))
}

impl CallbackContext {
    fn event(&mut self, record: &EVENT_RECORD) {
        let Some((direction, protocol)) = transfer_kind(record) else {
            return;
        };
        let descriptor = record.EventHeader.EventDescriptor;
        let key = SchemaKey {
            id: descriptor.Id,
            version: descriptor.Version,
            opcode: descriptor.Opcode,
            task: descriptor.Task,
            flags: record.EventHeader.Flags,
        };
        if !self.schemas.contains_key(&key) {
            if self.schemas.len() >= MAX_SCHEMA_COUNT {
                self.shared.data().schema_failure =
                    Some("网络提供者事件版本数量超出安全上限".into());
                return;
            }
            self.schemas.insert(key, NetworkSchema::discover(record));
        }
        let schema = &self.schemas[&key];
        let (pid, bytes) = match schema {
            Ok(schema) => match schema.decode(record) {
                Ok(values) => values,
                Err(_) => {
                    let mut state = self.shared.data();
                    state.incomplete_interval = true;
                    state.interval_error =
                        Some("网络事件载荷未能完整解码；本周期显示为不可用".into());
                    return;
                }
            },
            Err(error) => {
                let mut state = self.shared.data();
                state.schema_failure = Some(format!(
                    "事件 {} 版本 {} 无法解码：{error}",
                    key.id, key.version
                ));
                return;
            }
        };
        let Ok(timestamp) = u64::try_from(record.EventHeader.TimeStamp) else {
            return;
        };
        let mut state = self.shared.data();
        let count = match protocol {
            Protocol::TcpV4 => &mut state.protocol_events.tcp_ipv4,
            Protocol::TcpV6 => &mut state.protocol_events.tcp_ipv6,
            Protocol::UdpV4 => &mut state.protocol_events.udp_ipv4,
            Protocol::UdpV6 => &mut state.protocol_events.udp_ipv6,
        };
        *count = count.saturating_add(1);
        state.tallies.record(pid, timestamp, bytes, direction);
    }
}

fn transfer_kind(record: &EVENT_RECORD) -> Option<(Direction, Protocol)> {
    if record.EventHeader.ProviderId != KERNEL_NETWORK_PROVIDER {
        return None;
    }
    let descriptor = record.EventHeader.EventDescriptor;
    let direction = match (descriptor.Task, descriptor.Opcode) {
        (10, 10) | (11, 42) => Direction::Send,
        (10, 11) | (11, 43) => Direction::Receive,
        // Exclude TCP copy/connect/disconnect/retransmit events, which are not
        // another independent send/receive and would double-count payloads.
        _ => return None,
    };
    let protocol = match (descriptor.Task, descriptor.Keyword & NETWORK_KEYWORDS) {
        (10, 0x10) => Protocol::TcpV4,
        (10, 0x20) => Protocol::TcpV6,
        (11, 0x10) => Protocol::UdpV4,
        (11, 0x20) => Protocol::UdpV6,
        _ => return None,
    };
    Some((direction, protocol))
}

struct NetworkSchema {
    pid: ScalarProperty,
    size: ScalarProperty,
}

struct ScalarProperty {
    name: Vec<u16>,
    width: usize,
    /// Computed from this version's TDH metadata; never fixed wire offsets.
    fixed_offset: Option<usize>,
}

impl NetworkSchema {
    fn discover(record: &EVENT_RECORD) -> Result<Self, String> {
        let mut bytes = 0_u32;
        // SAFETY: record is valid for this synchronous TDH call; first query
        // obtains the required buffer size without an output pointer.
        let result = unsafe { TdhGetEventInformation(record, None, None, &mut bytes) };
        if result != ERROR_INSUFFICIENT_BUFFER.0 || bytes as usize > MAX_SCHEMA_BYTES {
            return Err(format!("TDH 元数据读取失败（{result}）"));
        }
        // u64 alignment supports TRACE_EVENT_INFO, its flexible property array
        // and UTF-16 offsets; Vec<u8> alignment is not guaranteed by Rust.
        let mut storage = vec![0_u64; (bytes as usize).div_ceil(size_of::<u64>())];
        let info = storage.as_mut_ptr().cast::<TRACE_EVENT_INFO>();
        // SAFETY: buffer is aligned, sized from TDH's required bytes, and kept
        // alive through all metadata and property-name reads below.
        let result = unsafe { TdhGetEventInformation(record, None, Some(info), &mut bytes) };
        if result != 0 {
            return Err(format!("TDH 元数据不可用（{result}）"));
        }
        if (bytes as usize) < size_of::<TRACE_EVENT_INFO>()
            || bytes as usize > size_of_val(storage.as_slice())
        {
            return Err("TDH 元数据头长度无效".into());
        }
        // SAFETY: TDH succeeded and the requested buffer contains the header.
        let info = unsafe { &*info };
        let length = bytes as usize;
        let properties_offset = std::mem::offset_of!(TRACE_EVENT_INFO, EventPropertyInfoArray);
        let count = info.TopLevelPropertyCount as usize;
        if length < properties_offset
            || count > info.PropertyCount as usize
            || count
                .checked_mul(size_of::<EVENT_PROPERTY_INFO>())
                .and_then(|size| properties_offset.checked_add(size))
                .is_none_or(|end| end > length)
        {
            return Err("TDH 属性范围无效".into());
        }
        let base = storage.as_ptr().cast::<u8>();
        // SAFETY: range validated above; top-level properties precede child
        // struct properties in TDH's property array.
        let properties = unsafe {
            std::slice::from_raw_parts(
                base.add(properties_offset).cast::<EVENT_PROPERTY_INFO>(),
                count,
            )
        };
        let mut offset = Some(0_usize);
        let mut pid = None;
        let mut size = None;
        for property in properties {
            let name = metadata_name(&storage, length, property.NameOffset)
                .ok_or_else(|| "TDH 属性名称范围无效".to_owned())?;
            let scalar = scalar_width(property);
            let field = String::from_utf16_lossy(&name[..name.len().saturating_sub(1)]);
            if field.eq_ignore_ascii_case("PID") || field.eq_ignore_ascii_case("ProcessId") {
                if scalar != Some(4) || pid.is_some() {
                    return Err("PID 字段不是唯一 UInt32".into());
                }
                pid = Some(ScalarProperty {
                    name,
                    width: 4,
                    fixed_offset: offset,
                });
            } else if field.eq_ignore_ascii_case("size") {
                let width = scalar
                    .filter(|size| *size == 4 || *size == 8)
                    .ok_or_else(|| "包字节字段不是 UInt32/UInt64".to_owned())?;
                if size.is_some() {
                    return Err("包字节字段不唯一".into());
                }
                size = Some(ScalarProperty {
                    name,
                    width,
                    fixed_offset: offset,
                });
            }
            offset = offset
                .zip(fixed_property_width(property))
                .and_then(|(offset, width)| offset.checked_add(width));
        }
        Ok(Self {
            pid: pid.ok_or("TDH 元数据缺少 PID 字段")?,
            size: size.ok_or("TDH 元数据缺少 size 字段")?,
        })
    }

    fn decode(&self, record: &EVENT_RECORD) -> Result<(u32, u64), String> {
        let pid = self
            .pid
            .read(record)
            .and_then(|value| value.try_into().ok())
            .ok_or("PID 字段解码失败")?;
        let bytes = self.size.read(record).ok_or("size 字段解码失败")?;
        Ok((pid, bytes))
    }
}

fn metadata_name(storage: &[u64], byte_length: usize, offset: u32) -> Option<Vec<u16>> {
    let offset = offset as usize;
    if offset == 0 || !offset.is_multiple_of(2) || offset >= byte_length {
        return None;
    }
    // SAFETY: offset/length are within the allocated u64 buffer, aligned for
    // u16. Search stays bounded if metadata has no NUL terminator.
    let units = unsafe {
        std::slice::from_raw_parts(
            storage.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
            (byte_length - offset) / 2,
        )
    };
    let end = units.iter().position(|unit| *unit == 0)?;
    Some(units[..=end].to_vec())
}

fn scalar_width(property: &EVENT_PROPERTY_INFO) -> Option<usize> {
    if property.Flags.0
        & (PropertyStruct.0
            | PropertyParamCount.0
            | PropertyParamLength.0
            | PropertyHasCustomSchema.0)
        != 0
    {
        return None;
    }
    // SAFETY: all union-selecting flags have been excluded. Scalar count and
    // primitive nonStructType are the active documented members.
    let (kind, count) = unsafe {
        (
            property.Anonymous1.nonStructType.InType,
            property.Anonymous2.count,
        )
    };
    if count != 1 {
        return None;
    }
    match kind as i32 {
        value if value == TDH_INTYPE_UINT32.0 => Some(4),
        value if value == TDH_INTYPE_UINT64.0 => Some(8),
        _ => None,
    }
}

fn fixed_property_width(property: &EVENT_PROPERTY_INFO) -> Option<usize> {
    if let Some(width) = scalar_width(property) {
        return Some(width);
    }
    // Remaining primitive fixed-width fields can precede PID/size in another
    // event version. Anything variable/structured falls back to TDH property API.
    if property.Flags.0
        & (PropertyStruct.0
            | PropertyParamCount.0
            | PropertyParamLength.0
            | PropertyHasCustomSchema.0)
        != 0
    {
        return None;
    }
    // SAFETY: flags select primitive type and fixed scalar count/length.
    let (kind, count, length) = unsafe {
        (
            property.Anonymous1.nonStructType.InType,
            property.Anonymous2.count,
            property.Anonymous3.length,
        )
    };
    if count != 1 {
        return None;
    }
    match kind as i32 {
        value if value == TDH_INTYPE_UINT8.0 || value == TDH_INTYPE_INT8.0 => Some(1),
        value if value == TDH_INTYPE_UINT16.0 || value == TDH_INTYPE_INT16.0 => Some(2),
        value
            if value == TDH_INTYPE_INT32.0
                || value == TDH_INTYPE_HEXINT32.0
                || value == TDH_INTYPE_FLOAT.0
                || value == TDH_INTYPE_BOOLEAN.0 =>
        {
            Some(4)
        }
        value
            if value == TDH_INTYPE_INT64.0
                || value == TDH_INTYPE_HEXINT64.0
                || value == TDH_INTYPE_DOUBLE.0
                || value == TDH_INTYPE_FILETIME.0 =>
        {
            Some(8)
        }
        value if value == TDH_INTYPE_GUID.0 => Some(16),
        value if value == TDH_INTYPE_BINARY.0 && length > 0 => Some(usize::from(length)),
        _ => None,
    }
}

impl ScalarProperty {
    fn read(&self, record: &EVENT_RECORD) -> Option<u64> {
        if let Some(offset) = self.fixed_offset {
            let end = offset.checked_add(self.width)?;
            if record.UserData.is_null() || end > usize::from(record.UserDataLength) {
                return None;
            }
            // SAFETY: ETW owns the payload during callback; computed TDH field
            // offset plus scalar width has been checked against payload length.
            let bytes = unsafe {
                std::slice::from_raw_parts(record.UserData.cast::<u8>().add(offset), self.width)
            };
            return decode_unsigned(bytes);
        }
        let property = PROPERTY_DATA_DESCRIPTOR {
            PropertyName: self.name.as_ptr() as u64,
            ArrayIndex: u32::MAX,
            Reserved: 0,
        };
        let mut length = 0_u32;
        // SAFETY: the cached UTF-16 field name is valid and record is live.
        if unsafe { TdhGetPropertySize(record, None, &[property], &mut length) } != 0
            || length as usize != self.width
        {
            return None;
        }
        let mut bytes = [0_u8; 8];
        // SAFETY: exact scalar output size has been confirmed by TDH.
        if unsafe { TdhGetProperty(record, None, &[property], &mut bytes[..self.width]) } != 0 {
            return None;
        }
        decode_unsigned(&bytes[..self.width])
    }
}

fn decode_unsigned(bytes: &[u8]) -> Option<u64> {
    match bytes.len() {
        4 => Some(u64::from(u32::from_le_bytes(bytes.try_into().ok()?))),
        8 => Some(u64::from_le_bytes(bytes.try_into().ok()?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: u16, version: u8, opcode: u8, task: u16, keyword: u64) -> EVENT_DESCRIPTOR {
        EVENT_DESCRIPTOR {
            Id: id,
            Version: version,
            Opcode: opcode,
            Task: task,
            Keyword: keyword,
            ..Default::default()
        }
    }

    fn record(payload: &mut [u8], descriptor: EVENT_DESCRIPTOR) -> EVENT_RECORD {
        EVENT_RECORD {
            EventHeader: EVENT_HEADER {
                Size: size_of::<EVENT_HEADER>() as u16,
                Flags: EVENT_HEADER_FLAG_64_BIT_HEADER as u16,
                ProviderId: KERNEL_NETWORK_PROVIDER,
                ProcessId: 9999, // Worker-header PID must never override payload.
                TimeStamp: 134_000_000_000_000_000,
                EventDescriptor: descriptor,
                ..Default::default()
            },
            UserDataLength: payload.len() as u16,
            UserData: payload.as_mut_ptr().cast(),
            ..Default::default()
        }
    }

    #[test]
    fn tdh_installed_metadata_decodes_all_four_protocols_and_directions() {
        // This invokes real TDH with the installed Microsoft provider metadata;
        // it validates schema parsing without requiring an elevated trace.
        // Synthetic event payloads do not constitute a live-traffic validation.
        let cases = [
            (10, 10, 10, 0x10, Direction::Send, Protocol::TcpV4),
            (11, 11, 10, 0x10, Direction::Receive, Protocol::TcpV4),
            (26, 10, 10, 0x20, Direction::Send, Protocol::TcpV6),
            (27, 11, 10, 0x20, Direction::Receive, Protocol::TcpV6),
            (42, 42, 11, 0x10, Direction::Send, Protocol::UdpV4),
            (43, 43, 11, 0x10, Direction::Receive, Protocol::UdpV4),
            (58, 42, 11, 0x20, Direction::Send, Protocol::UdpV6),
            (59, 43, 11, 0x20, Direction::Receive, Protocol::UdpV6),
        ];
        for (id, opcode, task, keyword, direction, protocol) in cases {
            let mut payload = [0_u8; 128];
            payload[..4].copy_from_slice(&42_u32.to_le_bytes());
            payload[4..8].copy_from_slice(&65_536_u32.to_le_bytes());
            let record = record(&mut payload, descriptor(id, 0, opcode, task, keyword));
            assert_eq!(transfer_kind(&record), Some((direction, protocol)));
            let schema = NetworkSchema::discover(&record).expect("real installed TDH metadata");
            assert_eq!(schema.decode(&record).unwrap(), (42, 65_536));
            // Check the variable-layout fallback against real TdhGetProperty too.
            let schema = NetworkSchema {
                pid: ScalarProperty {
                    fixed_offset: None,
                    ..schema.pid
                },
                size: ScalarProperty {
                    fixed_offset: None,
                    ..schema.size
                },
            };
            assert_eq!(schema.decode(&record).unwrap(), (42, 65_536));
        }
    }

    #[test]
    fn actual_elapsed_and_identity_changes_do_not_inherit_an_old_pids_bytes() {
        let mut tallies = Tallies::default();
        assert!(tallies.finish_interval(&[(42, 100)], 1.0, true).is_empty());
        tallies.record(42, 110, 1000, Direction::Receive);
        tallies.record(42, 111, 500, Direction::Send);
        let rates = tallies.finish_interval(&[(42, 100)], 2.5, true);
        assert_eq!(rates[&42].received_bytes_per_sec, 400.0);
        assert_eq!(rates[&42].sent_bytes_per_sec, 200.0);
        assert_eq!(rates[&42].process_start_time_windows_ticks, 100);
        assert_eq!(rates[&42].received_bytes, 1000);
        assert_eq!(rates[&42].sent_bytes, 500);
        tallies.record(42, 112, 9000, Direction::Receive);
        assert!(tallies.finish_interval(&[(42, 200)], 1.0, true).is_empty());
        tallies.record(42, 199, 90_000, Direction::Receive); // Delayed old process.
        tallies.record(42, 201, 100, Direction::Receive);
        let rates = tallies.finish_interval(&[(42, 200)], 0.25, true);
        assert_eq!(rates[&42].received_bytes, 100);
        assert_eq!(rates[&42].process_start_time_windows_ticks, 200);
        assert_eq!(rates[&42].received_bytes_per_sec, 400.0);
        let rates = tallies.finish_interval(&[(42, 200)], 1.0, true);
        assert_eq!(rates[&42].received_bytes_per_sec, 0.0);
        assert!(tallies.finish_interval(&[(42, 0)], 1.0, true).is_empty());
    }

    #[test]
    fn malformed_or_unknown_version_is_unavailable_instead_of_zero() {
        let shared = Arc::new(Shared::default());
        shared.data().phase = TracePhase::Running;
        let mut context = CallbackContext {
            shared: Arc::clone(&shared),
            schemas: HashMap::new(),
        };
        let mut payload = [0_u8; 4]; // PID exists but size is truncated.
        context.event(&record(&mut payload, descriptor(10, 0, 10, 10, 0x10)));
        assert!(shared.data().incomplete_interval);
        let mut payload = [0_u8; 128];
        context.event(&record(&mut payload, descriptor(10, 255, 10, 10, 0x10)));
        assert!(shared.data().schema_failure.is_some());
        let mut tallies = Tallies::default();
        tallies.finish_interval(&[(42, 100)], 1.0, true);
        tallies.record(42, 101, 1000, Direction::Receive);
        assert!(tallies.finish_interval(&[(42, 100)], 1.0, false).is_empty());
        assert!(trace_error("启动网络追踪", ERROR_ACCESS_DENIED.0).contains("权限"));
        assert!(trace_error("启用网络提供者", 1314).contains("不可用"));
    }

    #[test]
    fn loss_counters_and_nontransfer_events_do_not_produce_complete_rates() {
        let mut state = CollectorState {
            phase: TracePhase::Running,
            ..Default::default()
        };
        state.observe_losses(1, 0);
        assert!(state.incomplete_interval);
        state.incomplete_interval = false;
        state.observe_losses(1, 0);
        assert!(!state.incomplete_interval);
        state.observe_losses(1, 1);
        assert!(state.incomplete_interval);
        let mut payload = [0_u8; 128];
        for (id, opcode) in [(12, 12), (14, 14), (18, 18)] {
            let record = record(&mut payload, descriptor(id, 0, opcode, 10, 0x10));
            assert!(transfer_kind(&record).is_none());
        }
    }

    #[test]
    fn unsigned_byte_fields_keep_64_bit_precision_and_scalar_types() {
        assert_eq!(decode_unsigned(&u64::MAX.to_le_bytes()), Some(u64::MAX));
        assert_eq!(decode_unsigned(&[1, 2, 3]), None);
        let mut property = EVENT_PROPERTY_INFO::default();
        property.Anonymous1.nonStructType.InType = TDH_INTYPE_UINT64.0 as u16;
        property.Anonymous2.count = 1;
        assert_eq!(scalar_width(&property), Some(8));
        property.Flags = PropertyStruct;
        assert_eq!(scalar_width(&property), None);
    }

    /// Run deliberately from an already elevated test shell; never launches UAC.
    /// It creates only its own local TCP/UDP clients and listeners.
    #[test]
    #[ignore = "Requires existing ETW privilege; validates actual owned TCP/UDP IPv4/IPv6 loopback traffic"]
    fn live_owned_loopback_tcp_udp_ipv4_ipv6() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream, UdpSocket};
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: current process pseudo-handle and valid output FILETIMEs.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        }
        .unwrap();
        let birth = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let identities = [(std::process::id(), birth)];
        let mut monitor = ProcessNetworkMonitor::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = monitor.sample(&identities);
            if snapshot.available {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ETW never available: {}",
                snapshot.status
            );
            thread::sleep(Duration::from_millis(50));
        }
        for address in ["127.0.0.1:0", "[::1]:0"] {
            let listener = TcpListener::bind(address).unwrap();
            let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut receiver, _) = listener.accept().unwrap();
            receiver
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let payload = [0x5a_u8; 65_536];
            sender.write_all(&payload).unwrap();
            let mut received = [0_u8; 65_536];
            receiver.read_exact(&mut received).unwrap();
            assert_eq!(received, payload);
            let udp_receiver = UdpSocket::bind(address).unwrap();
            udp_receiver
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let udp_sender = UdpSocket::bind(address).unwrap();
            for _ in 0..16 {
                udp_sender
                    .send_to(&payload[..1024], udp_receiver.local_addr().unwrap())
                    .unwrap();
                let mut datagram = [0_u8; 2048];
                assert_eq!(udp_receiver.recv_from(&mut datagram).unwrap().0, 1024);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut sent = 0_u64;
        let mut received = 0_u64;
        let counts = loop {
            thread::sleep(Duration::from_millis(250));
            let snapshot = monitor.sample(&identities);
            assert!(snapshot.available, "{}", snapshot.status);
            if let Some(rate) = snapshot.rates.get(&std::process::id()) {
                sent += rate.sent_bytes;
                received += rate.received_bytes;
            }
            let counts = snapshot.protocol_events;
            if counts.tcp_ipv4 > 0
                && counts.tcp_ipv6 > 0
                && counts.udp_ipv4 > 0
                && counts.udp_ipv6 > 0
            {
                break counts;
            }
            assert!(
                Instant::now() < deadline,
                "Missing actual protocol events: {counts:?}"
            );
        };
        assert!(
            sent >= 65_536 * 2 + 1024 * 32,
            "Actual send bytes missing: {sent}"
        );
        assert!(
            received >= 65_536 * 2 + 1024 * 32,
            "Actual receive bytes missing: {received}"
        );
        eprintln!("Owned loopback sent={sent}, received={received}, protocols={counts:?}");
        let shared = Arc::clone(&monitor.shared);
        drop(monitor);
        assert!(shared.handles().consumer.is_none());
        assert!(shared.handles().controller.is_none());
    }
}

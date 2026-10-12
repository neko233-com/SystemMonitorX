//! On-demand, read-only diagnostics for one exact Windows process identity.
//!
//! Call on a worker, never for every process at each telemetry tick. Module
//! snapshots and memory queries are sequential observations, not an atomic
//! debugger snapshot. This module never reads memory contents or modifies the
//! target, enables privileges, suspends it, or duplicates its handles.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BAD_LENGTH, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES,
    ERROR_PARTIAL_COPY, FILETIME, GetLastError, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQueryEx};
use windows::Win32::System::SystemInformation::{
    GetNativeSystemInfo, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_UNKNOWN, SYSTEM_INFO,
};
use windows::Win32::System::Threading::{
    GetProcessId, GetProcessTimes, IsWow64Process2, OpenProcess, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

pub const MAX_MODULES: usize = 4_096;
pub const MAX_MEMORY_REGIONS: usize = 16_384;
pub const DEFAULT_BUDGET_MS: u64 = 2_000;

/// Cooperative limits checked between native calls. One Windows call itself
/// cannot be cancelled, so budget_ms is not a strict wall-clock deadline.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct DiagnosticLimits {
    pub max_modules: usize,
    pub max_memory_regions: usize,
    pub budget_ms: u64,
}
impl Default for DiagnosticLimits {
    fn default() -> Self {
        Self {
            max_modules: MAX_MODULES,
            max_memory_regions: MAX_MEMORY_REGIONS,
            budget_ms: DEFAULT_BUDGET_MS,
        }
    }
}
impl DiagnosticLimits {
    fn validate(self) -> Result<Self> {
        ensure!(
            (1..=MAX_MODULES).contains(&self.max_modules)
                && (1..=MAX_MEMORY_REGIONS).contains(&self.max_memory_regions)
                && (1..=10_000).contains(&self.budget_ms),
            "Diagnostic limits are outside the supported bounds"
        );
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnumerationStatus {
    Complete,
    Partial,
    Unavailable,
}

/// Stable semantic codes for localized UI. diagnostic preserves technical
/// API/error context; callers must not translate paths or raw OS errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticIssueCode {
    ModuleSnapshotScope,
    Wow64ModuleSnapshot,
    ArchitectureUnavailable,
    ModuleSnapshotFailed,
    ModuleEnumerationFailed,
    ModuleSnapshotChanging,
    ModuleLimit,
    MemoryRegionLimit,
    TimeLimit,
    CrossBitnessUnavailable,
    ModuleTextMayBeTruncated,
    UnexpectedModuleIdentity,
    MemoryAccessUnavailable,
    MemoryQueryFailed,
    InvalidMemoryRegion,
    SummaryOverflow,
    MemorySummaryScope,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticIssue {
    pub code: DiagnosticIssueCode,
    pub win32_error: Option<u32>,
    pub diagnostic: String,
}
impl DiagnosticIssue {
    fn note(code: DiagnosticIssueCode, diagnostic: impl Into<String>) -> Self {
        Self {
            code,
            win32_error: None,
            diagnostic: diagnostic.into(),
        }
    }
    fn error(code: DiagnosticIssueCode, api: &str, error: &windows::core::Error) -> Self {
        Self {
            code,
            win32_error: win32_code(error),
            diagnostic: format!("{api}: {error}"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LoadedModule {
    /// Original case and Unicode text from the Windows snapshot.
    pub name: String,
    pub path: String,
    /// Original UTF-16 units retained even when a malformed name needs lossy
    /// display decoding. This is not a local path inferred from the name.
    pub path_utf16: Vec<u16>,
    pub path_encoding_lossy: bool,
    pub path_may_be_truncated: bool,
    /// Target addresses are numbers only, never dereferenced locally.
    pub base_address: u64,
    pub image_size_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleReport {
    pub status: EnumerationStatus,
    pub entries: Vec<LoadedModule>,
    pub issues: Vec<DiagnosticIssue>,
    pub snapshot_attempts: u32,
}
impl Default for ModuleReport {
    fn default() -> Self {
        Self {
            status: EnumerationStatus::Unavailable,
            entries: Vec::new(),
            issues: Vec::new(),
            snapshot_attempts: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    Committed,
    Reserved,
    Free,
    Unknown,
}
impl MemoryState {
    pub fn from_raw(state: u32) -> Self {
        match state {
            0x1000 => Self::Committed,
            0x2000 => Self::Reserved,
            0x10000 => Self::Free,
            _ => Self::Unknown,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Private,
    Mapped,
    Image,
    Unknown,
}
impl MemoryKind {
    fn from_raw(kind: u32) -> Self {
        match kind {
            0x20000 => Self::Private,
            0x40000 => Self::Mapped,
            0x1000000 => Self::Image,
            _ => Self::Unknown,
        }
    }
}

/// The nominal page permission, not proof that an actual access would succeed.
/// Guarded pages raise an exception on first access. Reserved/free pages have
/// no defined current protection and are explicitly NotApplicable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAccess {
    Allowed,
    Denied,
    Guarded,
    Unknown,
    NotApplicable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryProtection {
    pub readable: PageAccess,
    pub writable: PageAccess,
    pub executable: PageAccess,
    pub guard: bool,
    pub copy_on_write: bool,
    pub no_cache: bool,
    pub write_combine: bool,
}
impl MemoryProtection {
    pub fn classify(state: MemoryState, raw: u32) -> Self {
        let mut result = Self {
            readable: PageAccess::NotApplicable,
            writable: PageAccess::NotApplicable,
            executable: PageAccess::NotApplicable,
            guard: false,
            copy_on_write: false,
            no_cache: false,
            write_combine: false,
        };
        if state != MemoryState::Committed {
            if state == MemoryState::Unknown {
                result.readable = PageAccess::Unknown;
                result.writable = PageAccess::Unknown;
                result.executable = PageAccess::Unknown;
            }
            return result;
        }
        result.guard = raw & 0x100 != 0;
        result.no_cache = raw & 0x200 != 0;
        result.write_combine = raw & 0x400 != 0;
        let base = raw & 0xff;
        result.copy_on_write = matches!(base, 0x08 | 0x80);
        let rights = match base {
            0x01 => Some((false, false, false)),
            0x02 => Some((true, false, false)),
            0x04 | 0x08 => Some((true, true, false)),
            0x10 => Some((false, false, true)),
            0x20 => Some((true, false, true)),
            0x40 | 0x80 => Some((true, true, true)),
            _ => None,
        };
        // Unknown modifiers (for example enclave-specific values) must not
        // silently inherit ordinary page-access semantics.
        let rights = if raw & !0x4000_07ff == 0 {
            rights
        } else {
            None
        };
        let access = |allowed: bool| {
            if !allowed {
                PageAccess::Denied
            } else if result.guard {
                PageAccess::Guarded
            } else {
                PageAccess::Allowed
            }
        };
        if let Some((read, write, execute)) = rights {
            result.readable = access(read);
            result.writable = access(write);
            result.executable = access(execute);
        } else {
            result.readable = PageAccess::Unknown;
            result.writable = PageAccess::Unknown;
            result.executable = PageAccess::Unknown;
        }
        result
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryRegion {
    pub base_address: u64,
    pub size_bytes: u64,
    /// Undefined for MEM_FREE, so not reported as zero or a valid allocation.
    pub allocation_base_address: Option<u64>,
    pub state: MemoryState,
    pub raw_state: u32,
    pub kind: Option<MemoryKind>,
    pub raw_kind: Option<u32>,
    /// Protect is undefined for free and reserved pages.
    pub raw_protection: Option<u32>,
    pub protection: MemoryProtection,
}
impl MemoryRegion {
    fn from_native(info: &MEMORY_BASIC_INFORMATION) -> Self {
        let state = MemoryState::from_raw(info.State.0);
        Self {
            base_address: info.BaseAddress as usize as u64,
            size_bytes: info.RegionSize as u64,
            allocation_base_address: (state != MemoryState::Free)
                .then_some(info.AllocationBase as usize as u64),
            state,
            raw_state: info.State.0,
            kind: (state != MemoryState::Free).then(|| MemoryKind::from_raw(info.Type.0)),
            raw_kind: (state != MemoryState::Free).then_some(info.Type.0),
            raw_protection: (state == MemoryState::Committed).then_some(info.Protect.0),
            protection: MemoryProtection::classify(state, info.Protect.0),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MemorySummary {
    pub region_count: u64,
    pub committed_bytes: u64,
    pub reserved_bytes: u64,
    pub free_bytes: u64,
    pub unknown_state_bytes: u64,
    /// These three allocation categories count committed pages only.
    pub private_committed_bytes: u64,
    pub mapped_committed_bytes: u64,
    pub image_committed_bytes: u64,
    pub unknown_kind_committed_bytes: u64,
    /// Permissions overlap (for example RX contributes to read and execute).
    /// Guarded pages are excluded from these nominal allowed-access totals.
    pub readable_committed_bytes: u64,
    pub writable_committed_bytes: u64,
    pub executable_committed_bytes: u64,
    pub guarded_committed_bytes: u64,
}
impl MemorySummary {
    fn append(&mut self, region: &MemoryRegion) -> Result<()> {
        // Apply atomically: an overflow cannot leave a half-counted region.
        let mut next = self.clone();
        let add = |destination: &mut u64, bytes: u64| -> Result<()> {
            *destination = destination
                .checked_add(bytes)
                .context("Memory summary overflow")?;
            Ok(())
        };
        add(&mut next.region_count, 1)?;
        match region.state {
            MemoryState::Committed => {
                add(&mut next.committed_bytes, region.size_bytes)?;
                let kind = match region.kind {
                    Some(MemoryKind::Private) => &mut next.private_committed_bytes,
                    Some(MemoryKind::Mapped) => &mut next.mapped_committed_bytes,
                    Some(MemoryKind::Image) => &mut next.image_committed_bytes,
                    _ => &mut next.unknown_kind_committed_bytes,
                };
                add(kind, region.size_bytes)?;
                if region.protection.readable == PageAccess::Allowed {
                    add(&mut next.readable_committed_bytes, region.size_bytes)?;
                }
                if region.protection.writable == PageAccess::Allowed {
                    add(&mut next.writable_committed_bytes, region.size_bytes)?;
                }
                if region.protection.executable == PageAccess::Allowed {
                    add(&mut next.executable_committed_bytes, region.size_bytes)?;
                }
                if region.protection.guard {
                    add(&mut next.guarded_committed_bytes, region.size_bytes)?;
                }
            }
            MemoryState::Reserved => add(&mut next.reserved_bytes, region.size_bytes)?,
            MemoryState::Free => add(&mut next.free_bytes, region.size_bytes)?,
            MemoryState::Unknown => add(&mut next.unknown_state_bytes, region.size_bytes)?,
        }
        *self = next;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryReport {
    pub status: EnumerationStatus,
    pub summary: MemorySummary,
    pub regions: Vec<MemoryRegion>,
    /// The next unscanned address. Totals are a prefix if status is Partial.
    pub scanned_through_address_exclusive: u64,
    pub issues: Vec<DiagnosticIssue>,
}
impl Default for MemoryReport {
    fn default() -> Self {
        Self {
            status: EnumerationStatus::Unavailable,
            summary: MemorySummary::default(),
            regions: Vec::new(),
            scanned_through_address_exclusive: 0,
            issues: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessDiagnostics {
    pub pid: u32,
    /// Decimal text in JSON preserves every original 100ns tick for consumers
    /// whose numeric type cannot represent integers above 2^53 exactly.
    #[serde(serialize_with = "serialize_creation_ticks")]
    pub creation_ticks: u64,
    /// Raw IMAGE_FILE_MACHINE codes from IsWow64Process2.
    pub process_machine: Option<u16>,
    pub native_machine: Option<u16>,
    pub is_wow64: Option<bool>,
    pub modules: ModuleReport,
    pub memory: MemoryReport,
    pub issues: Vec<DiagnosticIssue>,
    pub limits: DiagnosticLimits,
    pub read_duration_ms: u64,
}

fn serialize_creation_ticks<S: serde::Serializer>(
    ticks: &u64,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&ticks.to_string())
}

impl ProcessDiagnostics {
    pub fn read(pid: u32, exact_creation_ticks: u64) -> Result<Self> {
        Self::read_with_limits(pid, exact_creation_ticks, DiagnosticLimits::default())
    }

    pub fn read_with_limits(
        pid: u32,
        exact_creation_ticks: u64,
        limits: DiagnosticLimits,
    ) -> Result<Self> {
        ensure!(
            pid > 0 && exact_creation_ticks > 0,
            "Missing exact process PID or creation FILETIME"
        );
        let limits = limits.validate()?;
        let started = Instant::now();
        // SAFETY: read-only query and synchronization rights; RAII owns this
        // reference from the first identity check through the final check.
        let process = OwnedHandle(
            unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                )
            }
            .context(
                "Cannot read process diagnostics: process exited, access denied or protected",
            )?,
        );
        verify_identity(process.0, pid, exact_creation_ticks)?;
        let mut result = Self {
            pid,
            creation_ticks: exact_creation_ticks,
            process_machine: None,
            native_machine: None,
            is_wow64: None,
            modules: ModuleReport::default(),
            memory: MemoryReport::default(),
            issues: Vec::new(),
            limits,
            read_duration_ms: 0,
        };
        let (mut machine, mut native) =
            (IMAGE_FILE_MACHINE::default(), IMAGE_FILE_MACHINE::default());
        // SAFETY: valid owned query handle and writable machine outputs.
        match unsafe { IsWow64Process2(process.0, &mut machine, Some(&mut native)) } {
            Ok(()) => {
                result.process_machine = Some(machine.0);
                result.native_machine = Some(native.0);
                result.is_wow64 = Some(machine != IMAGE_FILE_MACHINE_UNKNOWN);
            }
            Err(error) => result.issues.push(DiagnosticIssue::error(
                DiagnosticIssueCode::ArchitectureUnavailable,
                "IsWow64Process2",
                &error,
            )),
        }
        result.modules = read_modules(pid, started, limits, result.is_wow64);
        verify_identity(process.0, pid, exact_creation_ticks)?;
        // A second handle requests only the additional documented query right.
        // A denied memory query does not discard a successful module snapshot.
        result.memory = match unsafe {
            OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE, false, pid)
        } {
            Ok(handle) => {
                let memory_process = OwnedHandle(handle);
                verify_identity(memory_process.0, pid, exact_creation_ticks)?;
                let memory = read_memory(memory_process.0, started, limits);
                verify_identity(memory_process.0, pid, exact_creation_ticks)?;
                memory
            }
            Err(error) => {
                let mut report = MemoryReport::default();
                report.issues.push(DiagnosticIssue::error(
                    DiagnosticIssueCode::MemoryAccessUnavailable,
                    "OpenProcess(PROCESS_QUERY_INFORMATION)",
                    &error,
                ));
                report
            }
        };
        verify_identity(process.0, pid, exact_creation_ticks)?;
        result.read_duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        Ok(result)
    }
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: exactly one successful OpenProcess/snapshot handle is owned.
        // Module hModule values and borrowed pseudo-handles are never closed.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn verify_identity(handle: HANDLE, expected_pid: u32, expected_birth: u64) -> Result<()> {
    // SAFETY: live owned process handle with query and synchronization rights.
    let actual_pid = unsafe { GetProcessId(handle) };
    if actual_pid == 0 {
        bail!("GetProcessId: {}", windows::core::Error::from_thread());
    }
    let (mut birth, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: valid writable FILETIMEs. The exit FILETIME is undefined for a
    // live process and is deliberately not used to infer whether it is alive.
    unsafe { GetProcessTimes(handle, &mut birth, &mut exit, &mut kernel, &mut user) }
        .context("Cannot verify exact process creation FILETIME")?;
    let actual_birth = (u64::from(birth.dwHighDateTime) << 32) | u64::from(birth.dwLowDateTime);
    ensure!(
        identity_matches(expected_pid, expected_birth, actual_pid, actual_birth),
        "Process identity changed: PID or original creation FILETIME does not match"
    );
    // SAFETY: zero-time query of the held process object's signaled state;
    // never waits or changes the target, unlike exit-code 259 heuristics.
    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_TIMEOUT => Ok(()),
        WAIT_OBJECT_0 => bail!("Process exited; diagnostics were discarded"),
        WAIT_FAILED => bail!(
            "Cannot verify process liveness: {}",
            windows::core::Error::from_thread()
        ),
        state => bail!("Unexpected process wait state: {}", state.0),
    }
}

fn identity_matches(
    expected_pid: u32,
    expected_birth: u64,
    actual_pid: u32,
    actual_birth: u64,
) -> bool {
    expected_pid > 0
        && expected_birth > 0
        && expected_pid == actual_pid
        && expected_birth == actual_birth
}

fn out_of_time(started: Instant, limits: DiagnosticLimits) -> bool {
    started.elapsed() >= Duration::from_millis(limits.budget_ms)
}

fn win32_code(error: &windows::core::Error) -> Option<u32> {
    let bits = error.code().0 as u32;
    (bits & 0xffff_0000 == 0x8007_0000).then_some(bits & 0xffff)
}

// Toolhelp fixed buffers cannot prove a full-capacity path is untruncated.
fn module_text(units: &[u16]) -> (String, Vec<u16>, bool, bool) {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let raw = units[..end].to_vec();
    let exact = String::from_utf16(&raw);
    let lossy = exact.is_err();
    (
        exact.unwrap_or_else(|_| String::from_utf16_lossy(&raw)),
        raw,
        lossy,
        end >= units.len().saturating_sub(1),
    )
}

fn read_modules(
    pid: u32,
    started: Instant,
    limits: DiagnosticLimits,
    wow64: Option<bool>,
) -> ModuleReport {
    let mut report = ModuleReport::default();
    report.issues.push(DiagnosticIssue::note(DiagnosticIssueCode::ModuleSnapshotScope,
        "Toolhelp loaded-image snapshot; excludes LOAD_LIBRARY_AS_DATAFILE and similar loads; not a complete list of mapped files or open files"));
    if wow64 == Some(true) {
        report.issues.push(DiagnosticIssue::note(DiagnosticIssueCode::Wow64ModuleSnapshot,
            "x64 caller requests native and 32-bit module snapshots with TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32"));
    }
    let mut snapshot = None;
    for attempt in 1..=3 {
        if out_of_time(started, limits) {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::TimeLimit,
                "Module snapshot time budget reached",
            ));
            return report;
        }
        report.snapshot_attempts = attempt;
        // SAFETY: no inherited handle, no heap/thread snapshot or target writes.
        match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) } {
            Ok(handle) => {
                snapshot = Some(OwnedHandle(handle));
                break;
            }
            Err(error) if win32_code(&error) == Some(ERROR_BAD_LENGTH.0) && attempt < 3 => {}
            Err(error) => {
                let code = match win32_code(&error) {
                    Some(code) if code == ERROR_PARTIAL_COPY.0 => {
                        DiagnosticIssueCode::CrossBitnessUnavailable
                    }
                    Some(code) if code == ERROR_BAD_LENGTH.0 => {
                        DiagnosticIssueCode::ModuleSnapshotChanging
                    }
                    _ => DiagnosticIssueCode::ModuleSnapshotFailed,
                };
                report.issues.push(DiagnosticIssue::error(
                    code,
                    "CreateToolhelp32Snapshot",
                    &error,
                ));
                return report;
            }
        }
    }
    let Some(snapshot) = snapshot else {
        return report;
    };
    let mut entry = MODULEENTRY32W {
        dwSize: size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: correct dwSize and live owned snapshot, fixed writable entry.
    if let Err(error) = unsafe { Module32FirstW(snapshot.0, &mut entry) } {
        if win32_code(&error) == Some(ERROR_NO_MORE_FILES.0) {
            report.status = EnumerationStatus::Complete;
        } else {
            report.issues.push(DiagnosticIssue::error(
                DiagnosticIssueCode::ModuleEnumerationFailed,
                "Module32FirstW",
                &error,
            ));
        }
        return report;
    }
    report.status = EnumerationStatus::Partial;
    let mut text_warning = false;
    loop {
        if entry.th32ProcessID != pid {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::UnexpectedModuleIdentity,
                "Module snapshot returned a different PID; remaining entries discarded",
            ));
            break;
        }
        let (name, _, _, name_short) = module_text(&entry.szModule);
        let (path, path_utf16, path_encoding_lossy, path_may_be_truncated) =
            module_text(&entry.szExePath);
        text_warning |= name_short || path_may_be_truncated || path_encoding_lossy;
        report.entries.push(LoadedModule {
            name,
            path,
            path_utf16,
            path_encoding_lossy,
            path_may_be_truncated,
            base_address: entry.modBaseAddr as usize as u64,
            image_size_bytes: u64::from(entry.modBaseSize),
        });
        // SAFETY: same fixed output entry and live snapshot. Asking for the next
        // entry before applying the storage bound avoids falsely truncating an
        // exactly-at-limit complete list. The extra entry is never stored.
        match unsafe { Module32NextW(snapshot.0, &mut entry) } {
            Err(error) if win32_code(&error) == Some(ERROR_NO_MORE_FILES.0) => {
                report.status = EnumerationStatus::Complete;
                break;
            }
            Err(error) => {
                report.issues.push(DiagnosticIssue::error(
                    DiagnosticIssueCode::ModuleEnumerationFailed,
                    "Module32NextW",
                    &error,
                ));
                break;
            }
            Ok(()) => {}
        }
        if report.entries.len() >= limits.max_modules {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::ModuleLimit,
                "Module entry limit reached; remaining modules were not stored",
            ));
            break;
        }
        if out_of_time(started, limits) {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::TimeLimit,
                "Module enumeration time budget reached",
            ));
            break;
        }
    }
    if text_warning {
        report.issues.push(DiagnosticIssue::note(DiagnosticIssueCode::ModuleTextMayBeTruncated,
            "Toolhelp fixed-size text may be truncated or need lossy UTF-16 display decoding; original path units are retained"));
    }
    report.entries.sort_by(|left, right| {
        crate::name_order::compare_names(&left.name, &right.name)
            .then(left.base_address.cmp(&right.base_address))
    });
    report
}

fn region_next_address(cursor: u64, base: u64, size: u64, upper: u64) -> Option<u64> {
    let next = base.checked_add(size)?;
    (size > 0 && base <= cursor && next > cursor && next <= upper).then_some(next)
}

fn read_memory(handle: HANDLE, started: Instant, limits: DiagnosticLimits) -> MemoryReport {
    let mut report = MemoryReport::default();
    report.issues.push(DiagnosticIssue::note(DiagnosticIssueCode::MemorySummaryScope,
        "Virtual address regions, not resident RAM or working set; image/mapped copy-on-write pages retain their mapping type; queries are not an atomic snapshot"));
    if usize::BITS != 64 {
        report.issues.push(DiagnosticIssue::note(
            DiagnosticIssueCode::CrossBitnessUnavailable,
            "Memory enumeration requires the supported 64-bit build",
        ));
        return report;
    }
    let mut system = SYSTEM_INFO::default();
    // SAFETY: correctly sized writable system information.
    unsafe { GetNativeSystemInfo(&mut system) };
    let Some(upper) = (system.lpMaximumApplicationAddress as usize as u64).checked_add(1) else {
        report.issues.push(DiagnosticIssue::note(
            DiagnosticIssueCode::InvalidMemoryRegion,
            "Invalid native address-space bound",
        ));
        return report;
    };
    let mut cursor = 0_u64;
    while cursor < upper {
        if report.regions.len() >= limits.max_memory_regions {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::MemoryRegionLimit,
                "Memory region limit reached; totals cover only the scanned prefix",
            ));
            break;
        }
        if out_of_time(started, limits) {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::TimeLimit,
                "Memory enumeration time budget reached; totals cover only the scanned prefix",
            ));
            break;
        }
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: numeric target address is never dereferenced locally. Query
        // writes only this native output struct and reads no memory contents.
        let bytes = unsafe {
            VirtualQueryEx(
                handle,
                Some(cursor as usize as *const c_void),
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if bytes == 0 {
            // Capture immediately, before any other Windows API can overwrite it.
            let error = unsafe { GetLastError() };
            if error == ERROR_INVALID_PARAMETER && !report.regions.is_empty() {
                // Documented result for an address above this target's range;
                // WOW64 may finish below the caller's native maximum.
                report.status = EnumerationStatus::Complete;
            } else {
                report.issues.push(DiagnosticIssue {
                    code: DiagnosticIssueCode::MemoryQueryFailed,
                    win32_error: Some(error.0),
                    diagnostic: format!(
                        "VirtualQueryEx at 0x{cursor:016X}: Windows error {}",
                        error.0
                    ),
                });
            }
            break;
        }
        if bytes != size_of::<MEMORY_BASIC_INFORMATION>() {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::InvalidMemoryRegion,
                format!(
                    "VirtualQueryEx returned {bytes} bytes instead of the native structure size"
                ),
            ));
            break;
        }
        let region = MemoryRegion::from_native(&info);
        let Some(next) = region_next_address(cursor, region.base_address, region.size_bytes, upper)
        else {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::InvalidMemoryRegion,
                "VirtualQueryEx returned a zero, overflowing, backward or out-of-range region",
            ));
            break;
        };
        if let Err(error) = report.summary.append(&region) {
            report.issues.push(DiagnosticIssue::note(
                DiagnosticIssueCode::SummaryOverflow,
                error.to_string(),
            ));
            break;
        }
        report.regions.push(region);
        cursor = next;
        report.scanned_through_address_exclusive = cursor;
        report.status = EnumerationStatus::Partial;
    }
    if cursor >= upper {
        report.status = EnumerationStatus::Complete;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(size_bytes: u64, raw_state: u32, raw_kind: u32, protection: u32) -> MemoryRegion {
        let state = MemoryState::from_raw(raw_state);
        MemoryRegion {
            base_address: 0,
            size_bytes,
            allocation_base_address: None,
            state,
            raw_state,
            kind: (state != MemoryState::Free).then(|| MemoryKind::from_raw(raw_kind)),
            raw_kind: (state != MemoryState::Free).then_some(raw_kind),
            raw_protection: (state == MemoryState::Committed).then_some(protection),
            protection: MemoryProtection::classify(state, protection),
        }
    }

    #[test]
    fn creation_ticks_json_preserves_exact_identity_above_javascript_safe_integer() {
        for birth in [9_007_199_254_740_993_u64, u64::MAX] {
            let report = ProcessDiagnostics {
                pid: 42,
                creation_ticks: birth,
                process_machine: None,
                native_machine: None,
                is_wow64: None,
                modules: ModuleReport::default(),
                memory: MemoryReport::default(),
                issues: Vec::new(),
                limits: DiagnosticLimits::default(),
                read_duration_ms: 0,
            };
            let typed_birth: u64 = report.creation_ticks;
            assert_eq!(typed_birth, birth);
            let json = serde_json::to_value(&report).unwrap();
            let decimal = birth.to_string();
            assert_eq!(json["creation_ticks"].as_str(), Some(decimal.as_str()));
            assert_eq!(json["pid"].as_u64(), Some(42));
            assert_eq!(
                json["creation_ticks"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
                birth
            );
        }
    }

    #[test]
    fn protection_classifies_execute_only_copy_on_write_and_guard_pages() {
        let execute = MemoryProtection::classify(MemoryState::Committed, 0x10);
        assert_eq!(execute.readable, PageAccess::Denied);
        assert_eq!(execute.writable, PageAccess::Denied);
        assert_eq!(execute.executable, PageAccess::Allowed);
        let copy = MemoryProtection::classify(MemoryState::Committed, 0x80 | 0x40000000);
        assert!(copy.copy_on_write);
        assert_eq!(copy.writable, PageAccess::Allowed);
        assert_eq!(copy.executable, PageAccess::Allowed);
        let guard = MemoryProtection::classify(MemoryState::Committed, 0x20 | 0x100);
        assert!(guard.guard);
        assert_eq!(guard.readable, PageAccess::Guarded);
        assert_eq!(guard.writable, PageAccess::Denied);
        assert_eq!(guard.executable, PageAccess::Guarded);
        let unknown = MemoryProtection::classify(MemoryState::Committed, 0x03);
        assert_eq!(unknown.readable, PageAccess::Unknown);
        assert_eq!(unknown.executable, PageAccess::Unknown);
        let enclave = MemoryProtection::classify(MemoryState::Committed, 0x10000002);
        assert_eq!(enclave.readable, PageAccess::Unknown);
        let cached = MemoryProtection::classify(MemoryState::Committed, 0x04 | 0x200);
        assert!(cached.no_cache);
        assert_eq!(cached.writable, PageAccess::Allowed);
    }

    #[test]
    fn undefined_free_and_reserved_protection_cannot_become_accessible() {
        for state in [MemoryState::Free, MemoryState::Reserved] {
            let result = MemoryProtection::classify(state, 0xffff_ffff);
            assert_eq!(result.readable, PageAccess::NotApplicable);
            assert_eq!(result.writable, PageAccess::NotApplicable);
            assert_eq!(result.executable, PageAccess::NotApplicable);
            assert!(!result.guard && !result.copy_on_write);
        }
    }

    #[test]
    fn summary_separates_virtual_states_mapping_types_and_overlapping_permissions() {
        let mut summary = MemorySummary::default();
        for entry in [
            region(4096, 0x1000, 0x20000, 0x04),
            region(8192, 0x1000, 0x1000000, 0x20),
            region(16384, 0x1000, 0x40000, 0x104),
            region(32768, 0x2000, 0x20000, 0x40),
            region(65536, 0x10000, 0x1000000, 0x40),
            region(128, 0x9999, 0x20000, 0x04),
        ] {
            summary.append(&entry).unwrap();
        }
        assert_eq!(summary.region_count, 6);
        assert_eq!(summary.committed_bytes, 28672);
        assert_eq!(summary.private_committed_bytes, 4096);
        assert_eq!(summary.image_committed_bytes, 8192);
        assert_eq!(summary.mapped_committed_bytes, 16384);
        assert_eq!(summary.readable_committed_bytes, 12288);
        assert_eq!(summary.writable_committed_bytes, 4096);
        assert_eq!(summary.executable_committed_bytes, 8192);
        assert_eq!(summary.guarded_committed_bytes, 16384);
        assert_eq!(summary.reserved_bytes, 32768);
        assert_eq!(summary.free_bytes, 65536);
        assert_eq!(summary.unknown_state_bytes, 128);
    }

    #[test]
    fn summary_overflow_does_not_commit_half_of_a_region() {
        let mut summary = MemorySummary {
            committed_bytes: u64::MAX,
            ..Default::default()
        };
        let original = summary.clone();
        assert!(summary.append(&region(1, 0x1000, 0x20000, 0x04)).is_err());
        assert_eq!(summary, original);
    }

    #[test]
    fn region_advance_rejects_zero_backwards_overflow_and_out_of_range() {
        assert_eq!(region_next_address(4096, 4096, 4096, 65536), Some(8192));
        assert_eq!(region_next_address(4096, 0, 8192, 65536), Some(8192));
        for (cursor, base, size, upper) in [
            (4096, 4096, 0, 65536),
            (4096, 8192, 4096, 65536),
            (4096, 0, 4096, 65536),
            (u64::MAX - 1, u64::MAX - 1, 2, u64::MAX),
            (4096, 4096, 8192, 8192),
        ] {
            assert_eq!(region_next_address(cursor, base, size, upper), None);
        }
    }

    #[test]
    fn identities_require_nonzero_pid_and_every_original_filetime_tick() {
        assert!(identity_matches(7, u64::MAX, 7, u64::MAX));
        assert!(!identity_matches(7, u64::MAX, 7, u64::MAX - 1));
        assert!(!identity_matches(7, 1, 8, 1));
        assert!(!identity_matches(0, 1, 0, 1));
        assert!(!identity_matches(7, 0, 7, 0));
        assert!(ProcessDiagnostics::read(0, 1).is_err());
        assert!(ProcessDiagnostics::read(7, 0).is_err());
    }

    #[test]
    fn module_text_preserves_whitespace_unicode_and_original_malformed_units() {
        let units: Vec<_> = " C:\\国际-é\\日本.dll ".encode_utf16().chain([0]).collect();
        let (text, raw, lossy, short) = module_text(&units);
        assert_eq!(text, " C:\\国际-é\\日本.dll ");
        assert_eq!(raw, units[..units.len() - 1]);
        assert!(!lossy);
        assert!(short);
        let (text, raw, lossy, _) = module_text(&[0xd800, 0]);
        assert!(lossy);
        assert_eq!(raw, [0xd800]);
        assert_eq!(text, "\u{fffd}");
        let (text, _, _, short) = module_text(&[65, 66]);
        assert_eq!(text, "AB");
        assert!(short);
    }

    #[test]
    fn limits_reject_unbounded_requests_and_allow_small_partial_probes() {
        assert!(DiagnosticLimits::default().validate().is_ok());
        for limits in [
            DiagnosticLimits {
                max_modules: 0,
                ..Default::default()
            },
            DiagnosticLimits {
                max_memory_regions: MAX_MEMORY_REGIONS + 1,
                ..Default::default()
            },
            DiagnosticLimits {
                budget_ms: 0,
                ..Default::default()
            },
            DiagnosticLimits {
                budget_ms: 10_001,
                ..Default::default()
            },
        ] {
            assert!(limits.validate().is_err());
        }
        assert!(
            DiagnosticLimits {
                max_modules: 1,
                max_memory_regions: 1,
                budget_ms: 1
            }
            .validate()
            .is_ok()
        );
    }
}

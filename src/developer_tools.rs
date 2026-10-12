//! Copyable commands and captured, selected-process diagnostic JSON.
//! No command execution, program launch, machine queries or file writes occur here.
//! https://learn.microsoft.com/powershell/module/microsoft.powershell.core/about/about_quoting_rules

use crate::{
    monitor::{ProcessSample, SystemSnapshot},
    network_ports::{PortEntry, PortSnapshot},
    process_details::ProcessDetails,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;
pub const MAX_SAVED_SEARCHES: usize = 8;
const WINDOWS_UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
const WINDOWS_MAX_DATETIME_TICKS: u64 = 2_650_467_743_999_999_999;

/// Native creation ticks are never substituted with rounded Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_ticks: Option<u64>,
}
impl ProcessIdentity {
    pub fn of(process: &ProcessSample) -> Self {
        Self {
            pid: process.pid,
            creation_ticks: process.start_time_windows_ticks.filter(|ticks| *ticks > 0),
        }
    }
    pub fn is_exact(self) -> bool {
        self.pid > 0 && self.creation_ticks.is_some_and(|ticks| ticks > 0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessCommandKind {
    PowerShellProcess,
    PowerShellTcp,
    PowerShellUdp,
    TaskList,
    PowerShellStop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableCommandKind {
    PowerShellLocation,
    CmdLocation,
    VisualStudioCode,
    WindowsTerminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandEffect {
    ReadOnly,
    ChangesDirectory,
    OpensApplication,
    TerminatesProcess,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyCommand {
    pub text: String,
    pub effect: CommandEffect,
    pub note: &'static str,
}

fn single_line(value: &str) -> bool {
    !value
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}
/// Apostrophes and smart single quotes are all PowerShell string delimiters.
/// Doubling them preserves literal arguments, dollar signs and expressions.
pub fn quote_powershell_literal(value: &str) -> Result<String> {
    ensure!(
        single_line(value),
        "A copied shell argument must be a single line without control characters"
    );
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        quoted.push(character);
        if matches!(character, '\'' | '\u{2018}' | '\u{2019}') {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    Ok(quoted)
}
/// Percent variables and delayed expansion are active even inside CMD quotes.
/// Reject them rather than assuming interactive and batch escaping are identical.
pub fn quote_cmd_path(value: &str) -> Result<String> {
    ensure!(
        !value.is_empty() && single_line(value),
        "A CMD path must be a nonempty single line"
    );
    ensure!(
        !value.contains(['"', '%', '!']),
        "This path cannot be represented safely in all CMD expansion modes; use PowerShell"
    );
    Ok(format!("\"{value}\""))
}
/// All generated text is clipboard-only. PowerShell holds one Process handle
/// while verifying exact birth. Tasklist cannot verify birth and states that limit.
pub fn process_command(identity: ProcessIdentity, kind: ProcessCommandKind) -> Result<CopyCommand> {
    ensure!(
        identity.pid > 0 && identity.pid <= i32::MAX as u32,
        "The selected PID is outside the supported command range"
    );
    if kind == ProcessCommandKind::TaskList {
        return Ok(CopyCommand {
            text: format!("tasklist.exe /FI \"PID eq {}\" /FO LIST", identity.pid),
            effect: CommandEffect::ReadOnly,
            note: "Tasklist queries the current numeric PID; it cannot verify creation time and PIDs can be reused.",
        });
    }
    let ticks = identity
        .creation_ticks
        .filter(|ticks| (WINDOWS_UNIX_EPOCH_TICKS..=WINDOWS_MAX_DATETIME_TICKS).contains(ticks));
    ensure!(
        ticks.is_some(),
        "An exact valid Windows creation time is required for this PowerShell command"
    );
    let ticks = ticks.expect("validated creation time");
    if kind == ProcessCommandKind::PowerShellStop {
        ensure!(
            identity.pid > 4 && identity.pid != std::process::id(),
            "A protected or monitor process cannot receive a stop command"
        );
    }
    let operation = match kind {
        ProcessCommandKind::PowerShellProcess => {
            "$smxProcess | Select-Object Id, ProcessName, StartTime, Path, CPU, WorkingSet64, VirtualMemorySize64, HandleCount, SessionId | Format-List"
        }
        ProcessCommandKind::PowerShellTcp => {
            "Get-NetTCPConnection -OwningProcess $smxProcess.Id -ErrorAction Stop | Select-Object LocalAddress, LocalPort, RemoteAddress, RemotePort, State, OwningProcess | Format-Table -AutoSize"
        }
        ProcessCommandKind::PowerShellUdp => {
            "Get-NetUDPEndpoint -OwningProcess $smxProcess.Id -ErrorAction Stop | Select-Object LocalAddress, LocalPort, OwningProcess | Format-Table -AutoSize"
        }
        ProcessCommandKind::PowerShellStop => {
            "Stop-Process -InputObject $smxProcess -Confirm -ErrorAction Stop"
        }
        ProcessCommandKind::TaskList => unreachable!("returned above"),
    };
    let text = format!(
        "& {{ $smxProcess = Get-Process -Id {} -ErrorAction Stop; try {{ $null = $smxProcess.Handle; if ($smxProcess.HasExited -or $smxProcess.StartTime.ToUniversalTime().ToFileTimeUtc() -ne [long]{ticks}) {{ throw 'The selected process exited or its PID was reused. Refresh SystemMonitorX.' }}; {operation} }} finally {{ $smxProcess.Dispose() }} }}",
        identity.pid
    );
    Ok(CopyCommand {
        text,
        effect: if kind == ProcessCommandKind::PowerShellStop {
            CommandEffect::TerminatesProcess
        } else {
            CommandEffect::ReadOnly
        },
        note: match kind {
            ProcessCommandKind::PowerShellTcp | ProcessCommandKind::PowerShellUdp => {
                "Checks exact process identity. Endpoint tables are independent current queries, not traffic or an atomic snapshot."
            }
            ProcessCommandKind::PowerShellStop => {
                "Copy only. Running this command verifies identity and asks for confirmation. Termination can discard unsaved work; descendants are not targeted."
            }
            _ => "Checks exact process identity; protected or exited processes can deny access.",
        },
    })
}
fn invalid_windows_component(component: &str) -> bool {
    if component.is_empty() || matches!(component, "." | "..") || component.ends_with([' ', '.']) {
        return true;
    }
    let basename = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        basename.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || basename
        .strip_prefix("COM")
        .or_else(|| basename.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}
/// Accept ordinary absolute DOS/UNC file paths. Reject device/provider paths,
/// traversal, wildcards and malformed components; no existence check is made.
fn executable_folder(executable: &Path) -> Result<String> {
    let raw = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("The executable path is not valid Unicode"))?;
    ensure!(
        single_line(raw) && !raw.contains(['<', '>', '"', '|', '?', '*']),
        "The executable path contains invalid Windows filename characters"
    );
    let normalized = raw.replace('/', "\\");
    let rest = if let Some(rest) = normalized.strip_prefix("\\\\") {
        ensure!(
            !rest.starts_with(['?', '.']),
            "Device paths are not supported by this copied command"
        );
        ensure!(
            !rest.contains(':') && rest.split('\\').count() >= 3,
            "A UNC executable path needs a server, share, and file without alternate streams"
        );
        rest
    } else {
        let bytes = normalized.as_bytes();
        ensure!(
            bytes.len() >= 4
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'\\',
            "An absolute Windows executable path is required"
        );
        let rest = &normalized[3..];
        ensure!(
            !rest.contains(':'),
            "Alternate streams and provider paths are not supported"
        );
        rest
    };
    ensure!(
        !rest.split('\\').any(invalid_windows_component),
        "The executable path contains a malformed or reserved component"
    );
    let parent = executable
        .parent()
        .and_then(Path::to_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("The executable path has no containing folder"))?;
    Ok(parent.to_owned())
}
pub fn executable_command(executable: &Path, kind: ExecutableCommandKind) -> Result<CopyCommand> {
    let folder = executable_folder(executable)?;
    let (text, effect, note) = match kind {
        ExecutableCommandKind::PowerShellLocation => (
            format!(
                "Set-Location -LiteralPath {} -ErrorAction Stop",
                quote_powershell_literal(&folder)?
            ),
            CommandEffect::ChangesDirectory,
            "The executable folder can differ from the process working directory.",
        ),
        ExecutableCommandKind::CmdLocation => {
            ensure!(
                !folder.starts_with("\\\\"),
                "CMD cd does not support UNC folders; use PowerShell"
            );
            (
                format!("cd /d {}", quote_cmd_path(&folder)?),
                CommandEffect::ChangesDirectory,
                "The executable folder can differ from the process working directory.",
            )
        }
        ExecutableCommandKind::VisualStudioCode => {
            // On Windows the code command commonly resolves to a CMD shim.
            // Percent/delayed expansion must not change the quoted argument.
            ensure!(
                !folder.contains(['%', '!']),
                "The Code CMD shim cannot represent this folder safely; copy its path instead"
            );
            (
                format!("code -- {}", quote_powershell_literal(&folder)?),
                CommandEffect::OpensApplication,
                "Requires the separately installed Visual Studio Code command.",
            )
        }
        ExecutableCommandKind::WindowsTerminal => (
            format!("wt.exe -d {}", quote_powershell_literal(&folder)?),
            CommandEffect::OpensApplication,
            "Requires separately installed Windows Terminal.",
        ),
    };
    Ok(CopyCommand { text, effect, note })
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ExportOptions {
    /// Original command lines can contain credentials; inclusion is explicit.
    pub include_command_line: bool,
}
/// Capture before a save dialog so later selection/refresh cannot change owners.
/// No host, environment or unrelated process inventory is exported.
pub struct DiagnosticCapture {
    snapshot: Arc<SystemSnapshot>,
    process_index: usize,
    ports: Option<Arc<PortSnapshot>>,
    details: Option<ProcessDetails>,
    inspection: Option<Value>,
}
impl DiagnosticCapture {
    pub fn for_process(
        snapshot: Arc<SystemSnapshot>,
        process_index: usize,
        ports: Option<Arc<PortSnapshot>>,
        details: Option<ProcessDetails>,
    ) -> Result<Self> {
        let process = snapshot.processes.get(process_index).ok_or_else(|| {
            anyhow::anyhow!("The selection is outside the captured process snapshot")
        })?;
        ensure!(process.pid > 0, "No process is selected");
        ensure!(
            snapshot
                .processes
                .iter()
                .filter(|other| other.pid == process.pid)
                .count()
                == 1,
            "The captured snapshot has ambiguous duplicate PIDs"
        );
        let identity = ProcessIdentity::of(process);
        if let Some(details) = &details {
            ensure!(
                identity.is_exact()
                    && details.pid == identity.pid
                    && Some(details.creation_ticks) == identity.creation_ticks,
                "Process details belong to another or unverifiable process instance"
            );
        }
        Ok(Self {
            snapshot,
            process_index,
            ports,
            details,
            inspection: None,
        })
    }
    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity::of(&self.snapshot.processes[self.process_index])
    }
    /// The caller supplies already captured modules/threads/affinity information.
    /// Reject contradictory owner fields and retain unknown provider source text.
    pub fn with_inspection(
        mut self,
        identity: ProcessIdentity,
        source: &str,
        payload: Value,
    ) -> Result<Self> {
        ensure!(
            identity.is_exact() && identity == self.identity(),
            "Inspection belongs to another or unverifiable process instance"
        );
        ensure!(payload.is_object(), "Inspection must be a JSON object");
        if let Some(pid) = payload.get("pid") {
            ensure!(
                pid.as_u64() == Some(u64::from(identity.pid)),
                "Inspection payload contains a different PID"
            );
        }
        for key in [
            "creation_ticks",
            "creation_time_windows_ticks",
            "exact_creation_ticks",
        ] {
            if let Some(ticks) = payload.get(key) {
                let actual = ticks
                    .as_u64()
                    .or_else(|| ticks.as_str().and_then(|value| value.parse::<u64>().ok()));
                ensure!(
                    actual == identity.creation_ticks,
                    "Inspection payload contains a different creation time"
                );
            }
        }
        self.inspection =
            Some(json!({"source": source, "identity": identity_json(identity), "data": payload}));
        Ok(self)
    }
    pub fn to_value(&self, options: ExportOptions) -> Value {
        self.to_value_at(options, SystemTime::now())
    }
    pub fn to_json_pretty(&self, options: ExportOptions) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.to_value(options))? + "\n")
    }
    fn to_value_at(&self, options: ExportOptions, exported_at: SystemTime) -> Value {
        let process = &self.snapshot.processes[self.process_index];
        let identity = self.identity();
        let (mut entries, mut unknown_owner_count, mut different_instance_count) =
            (Vec::new(), 0, 0);
        if let Some(ports) = &self.ports {
            for entry in ports
                .entries
                .iter()
                .filter(|entry| entry.pid == identity.pid)
            {
                match (
                    identity.creation_ticks,
                    entry.creation_ticks.filter(|ticks| *ticks > 0),
                ) {
                    (Some(expected), Some(actual)) if expected == actual => {
                        entries.push(endpoint_json(entry))
                    }
                    (Some(_), Some(_)) => different_instance_count += 1,
                    _ => unknown_owner_count += 1,
                }
            }
        }
        json!({
            "schema": "systemmonitorx.process_diagnostic", "schema_version": DIAGNOSTIC_SCHEMA_VERSION,
            "application": {"name": "SystemMonitorX", "version": env!("CARGO_PKG_VERSION")},
            "exported_at_unix_ms": unix_millis(exported_at), "process_sampled_at_unix_ms": unix_millis(self.snapshot.sampled_at),
            "process_source": "Windows process snapshot: sysinfo and native counters", "scope": "one explicitly selected captured process",
            "privacy": {"command_line_included": options.include_command_line && self.details.as_ref().is_some_and(|details| details.command_line.is_some()), "environment_included": false, "unrelated_processes_included": false},
            "identity": identity_json(identity), "process": process_json(process, self.snapshot.cpu_ready),
            "details": self.details.as_ref().map(|details| details_json(details, options)), "inspection": self.inspection,
            "endpoints": {
                "source": "Windows IP Helper owner-PID TCP/UDP tables", "collected_at_unix_ms": self.ports.as_ref().and_then(|ports| unix_millis(ports.collected_at)),
                "collection_complete": self.ports.as_ref().map(|ports| ports.complete), "collection_duration_ms": self.ports.as_ref().map(|ports| ports.collection_duration_ms),
                "identity_filter": "PID and exact Windows creation FILETIME; unverified ownership is excluded", "atomic_with_process_snapshot": false, "traffic_measurement": false,
                "udp_semantics": "bound endpoints; no remote address or connection state", "entries": entries,
                "excluded_unknown_owner_count": unknown_owner_count, "excluded_different_instance_count": different_instance_count,
                "issues": self.ports.as_ref().map(|ports| &ports.issues),
            },
            "units": {"memory": "bytes", "rates": "bytes per second", "cpu_usage": "percent of whole machine", "windows_ticks": "decimal strings of 100 ns ticks since 1601-01-01 UTC; not Unix seconds"},
            "notes": ["A captured process may already have exited. Commands and mutations require a fresh native identity check.", "Unavailable counters are null; measured zero remains zero.", "Version-resource company text is unverified, not an Authenticode publisher."],
        })
    }
}
fn unix_millis(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
}
fn identity_json(identity: ProcessIdentity) -> Value {
    json!({"pid": identity.pid, "creation_time_windows_ticks": identity.creation_ticks.map(|ticks| ticks.to_string()), "exact_creation_time_available": identity.is_exact()})
}
fn path_json(path: Option<&Path>) -> (Option<String>, Option<Vec<u16>>) {
    use std::os::windows::ffi::OsStrExt as _;
    match path {
        Some(path) => match path.to_str() {
            Some(value) => (Some(value.to_owned()), None),
            None => (None, Some(path.as_os_str().encode_wide().collect())),
        },
        None => (None, None),
    }
}
fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}
fn process_json(process: &ProcessSample, cpu_ready: bool) -> Value {
    let (executable_path, executable_path_utf16) = path_json(process.exe.as_deref());
    json!({
        "name": process.name, "parent_pid": process.parent_pid, "executable_path": executable_path, "executable_path_utf16_if_not_unicode": executable_path_utf16,
        "status_source_text": process.status, "start_time_unix_seconds": (process.start_time_unix_secs > 0).then_some(process.start_time_unix_secs), "run_time_seconds": (process.start_time_unix_secs > 0).then_some(process.run_time_secs),
        "cpu_usage_percent": finite(cpu_ready.then_some(f64::from(process.cpu_usage))), "cpu_time_seconds": finite(process.cpu_time_seconds), "cpu_time_windows_ticks": process.cpu_time_ticks.map(|ticks| ticks.to_string()),
        "memory_bytes": process.memory_bytes, "virtual_memory_bytes": process.virtual_memory_bytes, "working_set_bytes": process.working_set_bytes, "private_committed_bytes": process.private_bytes, "private_working_set_bytes": process.private_working_set_bytes,
        "thread_count": process.thread_count, "handle_count": process.handle_count, "session_id": process.session_id, "user_name": process.user_name,
        "disk_read_bytes_per_second": finite(process.disk_read_bytes_per_sec), "disk_write_bytes_per_second": finite(process.disk_write_bytes_per_sec), "total_disk_read_bytes": process.total_disk_read_bytes, "total_disk_write_bytes": process.total_disk_write_bytes,
        "gpu_usage_percent": finite(process.gpu_usage.map(f64::from)), "gpu_memory_bytes": process.gpu_memory_bytes,
        "network_received_bytes_per_second": finite(process.network_received_bytes_per_sec), "network_transmitted_bytes_per_second": finite(process.network_transmitted_bytes_per_sec),
    })
}
fn details_json(details: &ProcessDetails, options: ExportOptions) -> Value {
    json!({
        "identity": identity_json(ProcessIdentity { pid: details.pid, creation_ticks: Some(details.creation_ticks) }), "source": "Windows on-demand native process queries and executable version resources",
        "executable_path": details.executable_path, "command_line": if options.include_command_line { details.command_line.as_ref() } else { None },
        "architecture": details.architecture, "native_architecture": details.native_architecture, "is_wow64": details.is_wow64, "current_directory": details.current_directory,
        "priority_class": details.priority_class, "priority_source_text": details.priority, "efficiency_mode": details.efficiency_mode,
        "file_description": details.file_description, "company_name_unverified": details.company_name, "product_name": details.product_name,
        "file_version": details.file_version, "product_version": details.product_version, "original_filename": details.original_filename, "file_metadata_source_text": details.file_metadata_source,
        "issues": details.issues, "read_duration_ms": details.read_duration_ms,
    })
}
fn endpoint_json(entry: &PortEntry) -> Value {
    let (executable_path, executable_path_utf16) = path_json(entry.exe.as_deref());
    json!({
        "identity": identity_json(ProcessIdentity { pid: entry.pid, creation_ticks: entry.creation_ticks }), "process_name": entry.process_name, "executable_path": executable_path, "executable_path_utf16_if_not_unicode": executable_path_utf16,
        "protocol": entry.protocol.to_string(), "address_family": entry.family.to_string(), "local_address": entry.local.ip().to_string(), "local_port": entry.local.port(), "local_endpoint": entry.local.to_string(),
        "remote_address": entry.remote.map(|remote| remote.ip().to_string()), "remote_port": entry.remote.map(|remote| remote.port()), "remote_endpoint": entry.remote.map(|remote| remote.to_string()),
        "tcp_state": entry.tcp_state.map(|state| state.to_string()), "identity_note_source_text": entry.identity_note,
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    Processes,
    Ports,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub scope: SearchScope,
    pub name: String,
    pub query: String,
}
impl SavedSearch {
    pub fn new(scope: SearchScope, name: &str, query: &str) -> Result<Self> {
        ensure!(
            single_line(name) && single_line(query),
            "Saved searches cannot contain control characters or line breaks"
        );
        let (name, query) = (name.trim(), query.trim());
        ensure!(
            !name.is_empty() && name.chars().count() <= 60 && single_line(name),
            "A saved search name must contain 1–60 characters without line breaks"
        );
        ensure!(
            !query.is_empty() && query.chars().count() <= 256 && single_line(query),
            "A saved query must contain 1–256 characters without line breaks"
        );
        Ok(Self {
            scope,
            name: name.to_owned(),
            query: query.to_owned(),
        })
    }
}
/// Cap valid distinct entries and use the shared name A–Z rule. Interior spaces
/// in queries are preserved because they can belong to literal names and paths.
pub fn normalize_saved_searches(searches: Vec<SavedSearch>) -> Vec<SavedSearch> {
    let mut seen = HashSet::new();
    let mut valid: Vec<_> = searches
        .into_iter()
        .filter_map(|search| SavedSearch::new(search.scope, &search.name, &search.query).ok())
        .filter(|search| seen.insert((search.scope as u8, search.query.to_lowercase())))
        .take(MAX_SAVED_SEARCHES)
        .collect();
    valid.sort_by(|left, right| {
        crate::name_order::compare_names(&left.name, &right.name).then(left.query.cmp(&right.query))
    });
    valid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_ports::{IpFamily, PortProtocol, TcpState};
    use std::{path::PathBuf, time::Duration};
    const TICKS: u64 = 133_456_789_123_456_789;
    fn identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: 8123,
            creation_ticks: Some(TICKS),
        }
    }
    fn sample() -> ProcessSample {
        ProcessSample {
            pid: 8123,
            name: "国际-é-日本\"\napp.exe".into(),
            start_time_windows_ticks: Some(TICKS),
            exe: Some(PathBuf::from("C:\\国际-é-日本\\app.exe")),
            network_received_bytes_per_sec: Some(0.0),
            disk_read_bytes_per_sec: Some(f64::NAN),
            status: "future/raw status\r\n".into(),
            ..Default::default()
        }
    }
    fn snapshot(process: ProcessSample) -> Arc<SystemSnapshot> {
        Arc::new(SystemSnapshot {
            sampled_at: UNIX_EPOCH + Duration::from_millis(123_456),
            processes: vec![process],
            ..Default::default()
        })
    }
    fn endpoint(ticks: Option<u64>, port: u16) -> PortEntry {
        PortEntry {
            pid: identity().pid,
            creation_ticks: ticks,
            process_name: Some("raw 国际\"app".into()),
            exe: None,
            protocol: PortProtocol::Tcp,
            family: IpFamily::V4,
            local: format!("127.0.0.1:{port}").parse().unwrap(),
            remote: None,
            tcp_state: Some(TcpState::Listen),
            identity_note: None,
        }
    }
    #[test]
    fn powershell_quoting_preserves_apostrophes_unicode_and_literal_expressions() {
        assert_eq!(
            quote_powershell_literal("C:\\O'Brien\\国际 $env:USERPROFILE $(whoami) é").unwrap(),
            "'C:\\O''Brien\\国际 $env:USERPROFILE $(whoami) é'"
        );
        assert_eq!(
            quote_powershell_literal("C:\\d’angelo\\‘x’").unwrap(),
            "'C:\\d’’angelo\\‘‘x’’'"
        );
        assert_eq!(
            quote_powershell_literal("a; & echo x | \u{0060}value").unwrap(),
            "'a; & echo x | \u{0060}value'"
        );
        for value in ["a\0b", "a\nb", "a\rb", "a\tb", "a\u{2028}b"] {
            assert!(quote_powershell_literal(value).is_err());
        }
    }
    #[test]
    fn cmd_quoting_rejects_expansions_and_control_characters() {
        assert_eq!(
            quote_cmd_path("C:\\国际 é\\O'Brien & tools").unwrap(),
            "\"C:\\国际 é\\O'Brien & tools\""
        );
        for value in [
            "",
            "C:\\%USERPROFILE%",
            "C:\\!task!",
            "C:\\a\"b",
            "C:\\a\nb",
            "C:\\a\0b",
        ] {
            assert!(quote_cmd_path(value).is_err());
        }
    }
    #[test]
    fn executable_commands_use_containing_folder_and_correct_shell_quoting() {
        let path = Path::new("C:\\O'Brien\\国际-é-日本\\app.exe");
        assert_eq!(
            executable_command(path, ExecutableCommandKind::PowerShellLocation)
                .unwrap()
                .text,
            "Set-Location -LiteralPath 'C:\\O''Brien\\国际-é-日本' -ErrorAction Stop"
        );
        assert_eq!(
            executable_command(path, ExecutableCommandKind::CmdLocation)
                .unwrap()
                .text,
            "cd /d \"C:\\O'Brien\\国际-é-日本\""
        );
        assert_eq!(
            executable_command(path, ExecutableCommandKind::VisualStudioCode)
                .unwrap()
                .text,
            "code -- 'C:\\O''Brien\\国际-é-日本'"
        );
        assert_eq!(
            executable_command(
                Path::new("C:\\app.exe"),
                ExecutableCommandKind::PowerShellLocation
            )
            .unwrap()
            .text,
            "Set-Location -LiteralPath 'C:\\' -ErrorAction Stop"
        );
        assert!(
            executable_command(
                Path::new("\\\\server\\share\\folder\\app.exe"),
                ExecutableCommandKind::PowerShellLocation
            )
            .unwrap()
            .text
            .contains("'\\\\server\\share\\folder'")
        );
        assert!(
            executable_command(
                Path::new("\\\\server\\share\\app.exe"),
                ExecutableCommandKind::CmdLocation
            )
            .is_err()
        );
    }
    #[test]
    fn malformed_executable_paths_never_produce_shell_commands() {
        for raw in [
            "app.exe",
            "C:app.exe",
            "HKCU:\\app.exe",
            "C:\\a\\..\\app.exe",
            "C:\\a\\.\\app.exe",
            "C:\\bad \\app.exe",
            "C:\\bad.\\app.exe",
            "C:\\CON\\app.exe",
            "C:\\COM1.txt\\app.exe",
            "C:\\a\\app.exe:stream",
            "C:\\a\\*.exe",
            "C:\\a\\\napp.exe",
            "\\\\?\\C:\\app.exe",
            "\\\\.\\pipe\\app.exe",
            "\\\\server\\app.exe",
            "C:\\a\\",
        ] {
            assert!(
                executable_command(Path::new(raw), ExecutableCommandKind::PowerShellLocation)
                    .is_err(),
                "{raw}"
            );
        }
        assert!(
            executable_command(
                Path::new("C:\\a%foo%\\app.exe"),
                ExecutableCommandKind::CmdLocation
            )
            .is_err()
        );
        assert!(
            executable_command(
                Path::new("C:\\a%foo%\\app.exe"),
                ExecutableCommandKind::PowerShellLocation
            )
            .is_ok()
        );
    }
    #[test]
    fn process_commands_pin_handle_and_compare_exact_creation_time() {
        for kind in [
            ProcessCommandKind::PowerShellProcess,
            ProcessCommandKind::PowerShellTcp,
            ProcessCommandKind::PowerShellUdp,
            ProcessCommandKind::PowerShellStop,
        ] {
            let command = process_command(identity(), kind).unwrap();
            assert!(
                command
                    .text
                    .contains("Get-Process -Id 8123 -ErrorAction Stop")
            );
            assert!(command.text.contains("$null = $smxProcess.Handle"));
            assert!(
                command
                    .text
                    .contains("ToFileTimeUtc() -ne [long]133456789123456789")
            );
            assert!(command.text.contains("finally { $smxProcess.Dispose() }"));
            assert!(!command.text.contains(" -Force") && !command.text.contains("taskkill"));
            assert_eq!(
                command.effect == CommandEffect::TerminatesProcess,
                kind == ProcessCommandKind::PowerShellStop
            );
            if kind == ProcessCommandKind::PowerShellStop {
                assert!(
                    command.text.contains(
                        "Stop-Process -InputObject $smxProcess -Confirm -ErrorAction Stop"
                    )
                );
            }
        }
        let tasklist = process_command(identity(), ProcessCommandKind::TaskList).unwrap();
        assert_eq!(tasklist.text, "tasklist.exe /FI \"PID eq 8123\" /FO LIST");
        assert!(tasklist.note.contains("PIDs can be reused"));
    }
    #[test]
    fn process_commands_reject_unknown_birth_invalid_pid_and_protected_stops() {
        for creation_ticks in [None, Some(0), Some(1), Some(u64::MAX)] {
            assert!(
                process_command(
                    ProcessIdentity {
                        creation_ticks,
                        ..identity()
                    },
                    ProcessCommandKind::PowerShellStop
                )
                .is_err()
            );
        }
        for pid in [0, 4, std::process::id(), u32::MAX] {
            assert!(
                process_command(
                    ProcessIdentity { pid, ..identity() },
                    ProcessCommandKind::PowerShellStop
                )
                .is_err()
            );
        }
        assert!(
            process_command(
                ProcessIdentity {
                    creation_ticks: None,
                    ..identity()
                },
                ProcessCommandKind::TaskList
            )
            .is_ok()
        );
        assert!(
            process_command(
                ProcessIdentity {
                    creation_ticks: None,
                    ..identity()
                },
                ProcessCommandKind::PowerShellProcess
            )
            .is_err()
        );
    }
    #[test]
    fn diagnostic_json_roundtrips_raw_unicode_controls_and_unavailable_counters() {
        let capture = DiagnosticCapture::for_process(snapshot(sample()), 0, None, None).unwrap();
        let report = capture.to_value_at(
            ExportOptions::default(),
            UNIX_EPOCH + Duration::from_millis(555),
        );
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["process_sampled_at_unix_ms"], 123456);
        assert_eq!(report["exported_at_unix_ms"], 555);
        assert_eq!(
            report["identity"]["creation_time_windows_ticks"],
            TICKS.to_string()
        );
        assert_eq!(report["process"]["name"], sample().name);
        assert_eq!(report["process"]["status_source_text"], sample().status);
        assert_eq!(report["process"]["network_received_bytes_per_second"], 0.0);
        assert!(report["process"]["disk_read_bytes_per_second"].is_null());
        assert!(report["process"]["cpu_usage_percent"].is_null());
        assert!(
            report["details"].is_null() && report["endpoints"]["collection_complete"].is_null()
        );
        assert!(report["process"]["handle_count"].is_null());
        let encoded = serde_json::to_string_pretty(&report).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), report);
        assert!(!encoded.contains("host_name") && !encoded.contains("environment_variables"));
    }
    #[test]
    fn unknown_birth_is_not_replaced_with_unix_seconds_or_pid_only_owners() {
        let process = ProcessSample {
            start_time_windows_ticks: None,
            start_time_unix_secs: 1_700_000_000,
            ..sample()
        };
        let ports = Arc::new(PortSnapshot {
            entries: vec![endpoint(None, 8080), endpoint(Some(TICKS), 8081)],
            ..Default::default()
        });
        let capture =
            DiagnosticCapture::for_process(snapshot(process), 0, Some(ports), None).unwrap();
        let value = capture.to_value(ExportOptions::default());
        assert!(value["identity"]["creation_time_windows_ticks"].is_null());
        assert_eq!(value["identity"]["exact_creation_time_available"], false);
        assert_eq!(value["endpoints"]["entries"], json!([]));
        assert_eq!(value["endpoints"]["excluded_unknown_owner_count"], 2);
    }
    #[test]
    fn endpoint_export_excludes_unknown_and_reused_pid_instances() {
        let mut unrelated = endpoint(Some(TICKS), 8083);
        unrelated.pid += 1;
        let ports = Arc::new(PortSnapshot {
            entries: vec![
                endpoint(Some(TICKS), 8080),
                endpoint(Some(TICKS + 1), 8081),
                endpoint(None, 8082),
                unrelated,
            ],
            complete: true,
            ..Default::default()
        });
        let capture =
            DiagnosticCapture::for_process(snapshot(sample()), 0, Some(ports), None).unwrap();
        let value = capture.to_value(ExportOptions::default());
        assert_eq!(value["endpoints"]["entries"].as_array().unwrap().len(), 1);
        assert_eq!(value["endpoints"]["entries"][0]["local_port"], 8080);
        assert_eq!(value["endpoints"]["excluded_unknown_owner_count"], 1);
        assert_eq!(value["endpoints"]["excluded_different_instance_count"], 1);
        assert_eq!(value["endpoints"]["traffic_measurement"], false);
        assert_eq!(value["endpoints"]["atomic_with_process_snapshot"], false);
    }
    #[test]
    fn command_line_requires_explicit_export_option_and_matched_details() {
        let details = ProcessDetails {
            pid: identity().pid,
            creation_ticks: TICKS,
            command_line: Some("app.exe --token=PRIVATE\"国际".into()),
            ..Default::default()
        };
        let capture =
            DiagnosticCapture::for_process(snapshot(sample()), 0, None, Some(details.clone()))
                .unwrap();
        let normal = capture.to_value(ExportOptions::default());
        assert!(normal["details"]["command_line"].is_null());
        assert!(!serde_json::to_string(&normal).unwrap().contains("PRIVATE"));
        assert_eq!(
            capture.to_value(ExportOptions {
                include_command_line: true
            })["details"]["command_line"],
            details.command_line.unwrap()
        );
        assert!(
            DiagnosticCapture::for_process(
                snapshot(sample()),
                0,
                None,
                Some(ProcessDetails {
                    pid: 1,
                    creation_ticks: TICKS,
                    ..Default::default()
                })
            )
            .is_err()
        );
        assert!(
            DiagnosticCapture::for_process(
                snapshot(sample()),
                0,
                None,
                Some(ProcessDetails {
                    pid: identity().pid,
                    creation_ticks: TICKS + 1,
                    ..Default::default()
                })
            )
            .is_err()
        );
    }
    #[test]
    fn capture_rejects_invalid_selection_and_duplicate_pids() {
        assert!(DiagnosticCapture::for_process(snapshot(sample()), 1, None, None).is_err());
        let duplicate = Arc::new(SystemSnapshot {
            processes: vec![
                sample(),
                ProcessSample {
                    start_time_windows_ticks: Some(TICKS + 1),
                    ..sample()
                },
            ],
            ..Default::default()
        });
        assert!(DiagnosticCapture::for_process(duplicate, 0, None, None).is_err());
        assert!(
            DiagnosticCapture::for_process(
                snapshot(ProcessSample { pid: 0, ..sample() }),
                0,
                None,
                None
            )
            .is_err()
        );
    }
    #[test]
    fn inspection_rejects_wrong_birth_and_preserves_unknown_source_text() {
        let captured =
            || DiagnosticCapture::for_process(snapshot(sample()), 0, None, None).unwrap();
        assert!(
            captured()
                .with_inspection(
                    ProcessIdentity {
                        creation_ticks: Some(TICKS + 1),
                        ..identity()
                    },
                    "new",
                    json!({})
                )
                .is_err()
        );
        assert!(
            captured()
                .with_inspection(identity(), "new", json!({"pid": 9}))
                .is_err()
        );
        assert!(
            captured()
                .with_inspection(identity(), "new", json!({"creation_ticks": TICKS + 1}))
                .is_err()
        );
        assert!(
            captured()
                .with_inspection(identity(), "new", json!([]))
                .is_err()
        );
        let value = captured()
            .with_inspection(
                identity(),
                "future/provider 国际\"\r\n",
                json!({"pid": identity().pid, "creation_ticks": TICKS.to_string(), "modules": []}),
            )
            .unwrap()
            .to_value(ExportOptions::default());
        assert_eq!(value["inspection"]["source"], "future/provider 国际\"\r\n");
        assert_eq!(value["inspection"]["data"]["modules"], json!([]));
    }
    #[test]
    fn non_unicode_path_is_preserved_as_original_utf16_for_export() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt as _};
        let wide = [67, 58, 92, 0xd800, 92, 97];
        let path = PathBuf::from(OsString::from_wide(&wide));
        assert!(executable_command(&path, ExecutableCommandKind::PowerShellLocation).is_err());
        let capture = DiagnosticCapture::for_process(
            snapshot(ProcessSample {
                exe: Some(path),
                ..sample()
            }),
            0,
            None,
            None,
        )
        .unwrap();
        let value = capture.to_value(ExportOptions::default());
        assert!(value["process"]["executable_path"].is_null());
        assert_eq!(
            value["process"]["executable_path_utf16_if_not_unicode"],
            json!(wide)
        );
    }
    #[test]
    fn saved_searches_are_bounded_sorted_and_preserve_query_interior_spaces() {
        let mut searches = vec![
            SavedSearch::new(
                SearchScope::Processes,
                "Zulu",
                " path:C:\\Program  Files\\国际 ",
            )
            .unwrap(),
            SavedSearch::new(SearchScope::Ports, "Alpha", "tcp listen").unwrap(),
            SavedSearch {
                scope: SearchScope::Ports,
                name: "Duplicate".into(),
                query: "TCP LISTEN".into(),
            },
            SavedSearch {
                scope: SearchScope::Processes,
                name: "Invalid".into(),
                query: "x\ncommand".into(),
            },
        ];
        for index in 0..20 {
            searches.push(
                SavedSearch::new(
                    SearchScope::Processes,
                    &format!("fixture {index}"),
                    &format!("pid:{}", index + 1),
                )
                .unwrap(),
            );
        }
        let valid = normalize_saved_searches(searches);
        assert_eq!(valid.len(), MAX_SAVED_SEARCHES);
        assert_eq!(valid[0].name, "Alpha");
        assert_eq!(
            valid
                .iter()
                .find(|search| search.name == "Zulu")
                .unwrap()
                .query,
            "path:C:\\Program  Files\\国际"
        );
        assert_eq!(
            valid
                .iter()
                .filter(|search| search.scope == SearchScope::Ports)
                .count(),
            1
        );
        assert!(SavedSearch::new(SearchScope::Ports, "", "tcp").is_err());
        assert!(SavedSearch::new(SearchScope::Ports, "name", "").is_err());
        assert!(SavedSearch::new(SearchScope::Ports, "name", "\ntcp listen\n").is_err());
        assert!(SavedSearch::new(SearchScope::Ports, "name", &"x".repeat(257)).is_err());
        let serialized = serde_json::to_string(&valid).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<SavedSearch>>(&serialized).unwrap(),
            valid
        );
    }
}

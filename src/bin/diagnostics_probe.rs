//! Headless, read-only smoke probe for native process diagnostics.
//! Reads only this executable's own process; accepts no external PID.

#[cfg(windows)]
mod windows_probe {
    use anyhow::{Context, Result, bail, ensure};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use system_monitor_x::process_diagnostics::{
        DiagnosticIssueCode, DiagnosticLimits, EnumerationStatus, ProcessDiagnostics,
    };
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn own_creation_ticks() -> Result<u64> {
        let (mut birth, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: borrowed own-process pseudo-handle is never closed; writable
        // local FILETIMEs. Exit time is not used to infer liveness.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut birth,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        }
        .context("Cannot capture probe's own creation FILETIME")?;
        Ok((u64::from(birth.dwHighDateTime) << 32) | u64::from(birth.dwLowDateTime))
    }

    pub fn run() -> Result<()> {
        let mut smoke = false;
        let mut output = None::<PathBuf>;
        let mut arguments = std::env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--smoke") => smoke = true,
                Some("--output") => {
                    let path = arguments.next().context("--output requires a file path")?;
                    ensure!(!path.is_empty(), "--output path must not be empty");
                    output = Some(path.into());
                }
                Some("--help" | "-h") => {
                    println!(
                        "diagnostics_probe [--smoke] [--output FILE]\n\nReads loaded modules and virtual-memory metadata for this executable only.\nNever reads memory contents or mutates any process. No external PID accepted.\n--smoke validates native results, exact identity rejection and partial limits.\n--output also saves the private diagnostic JSON report to FILE."
                    );
                    return Ok(());
                }
                _ => bail!("Unknown argument: {}", argument.to_string_lossy()),
            }
        }

        let pid = std::process::id();
        let birth = own_creation_ticks()?;
        let diagnostics = ProcessDiagnostics::read(pid, birth)?;
        let stale_birth = if birth == u64::MAX {
            birth - 1
        } else {
            birth + 1
        };
        let stale_identity_rejected = ProcessDiagnostics::read(pid, stale_birth).is_err();
        let partial = ProcessDiagnostics::read_with_limits(
            pid,
            birth,
            DiagnosticLimits {
                max_modules: 1,
                max_memory_regions: 1,
                budget_ms: 10_000,
            },
        )?;
        let executable = std::env::current_exe().context("Cannot read own executable path")?;
        let executable_name = executable
            .file_name()
            .context("Own executable has no file name")?
            .to_string_lossy();
        let own_module_found = diagnostics.modules.entries.iter().any(|module| {
            module.name.eq_ignore_ascii_case(&executable_name) && module.image_size_bytes > 0
        });
        let summary = &diagnostics.memory.summary;
        let mapping_total = summary
            .private_committed_bytes
            .checked_add(summary.mapped_committed_bytes)
            .and_then(|value| value.checked_add(summary.image_committed_bytes))
            .and_then(|value| value.checked_add(summary.unknown_kind_committed_bytes));
        let checks: Vec<Value> = vec![
            json!({"name": "exact_self_identity", "passed": diagnostics.pid == pid && diagnostics.creation_ticks == birth}),
            json!({"name": "stale_filetime_rejected", "passed": stale_identity_rejected}),
            json!({"name": "own_executable_module_found", "passed": own_module_found}),
            json!({"name": "module_snapshot_complete", "passed": diagnostics.modules.status == EnumerationStatus::Complete}),
            json!({"name": "virtual_memory_complete", "passed": diagnostics.memory.status == EnumerationStatus::Complete}),
            json!({"name": "committed_mapping_totals_match", "passed": mapping_total == Some(summary.committed_bytes) && summary.committed_bytes > 0}),
            json!({"name": "memory_region_count_matches", "passed": summary.region_count as usize == diagnostics.memory.regions.len() && !diagnostics.memory.regions.is_empty()}),
            json!({"name": "memory_has_nominal_read_and_execute", "passed": summary.readable_committed_bytes > 0 && summary.executable_committed_bytes > 0}),
            json!({"name": "bounded_modules_report_partial", "passed":
                partial.modules.entries.len() == 1 &&
                partial.modules.status == EnumerationStatus::Partial &&
                partial.modules.issues.iter().any(|issue| issue.code == DiagnosticIssueCode::ModuleLimit)}),
            json!({"name": "bounded_memory_reports_partial", "passed":
                partial.memory.regions.len() == 1 &&
                partial.memory.status == EnumerationStatus::Partial &&
                partial.memory.issues.iter().any(|issue| issue.code == DiagnosticIssueCode::MemoryRegionLimit)}),
        ];
        let passed = checks.iter().all(|check| check["passed"] == true);
        let report = json!({
            "probe": "own_process_only",
            "version": env!("CARGO_PKG_VERSION"),
            "passed": passed,
            "checks": checks,
            "diagnostics": diagnostics,
            "partial": partial,
        });
        let bytes = serde_json::to_vec_pretty(&report)?;
        if let Some(output) = output {
            std::fs::write(&output, &bytes)
                .with_context(|| format!("Cannot write {}", output.display()))?;
        }
        // Explicit stdout is suitable for local diagnostics; do not publish
        // reports containing local paths or target virtual addresses by default.
        println!("{}", String::from_utf8(bytes)?);
        ensure!(
            !smoke || passed,
            "One or more own-process diagnostic smoke checks failed"
        );
        Ok(())
    }
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_probe::run() {
        eprintln!("diagnostics_probe: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("diagnostics_probe requires Windows");
    std::process::exit(1);
}

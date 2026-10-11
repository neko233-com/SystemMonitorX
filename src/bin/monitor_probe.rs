//! Headless diagnostics for real Windows collectors and owned-child operations.
//! Example: cargo run --bin monitor_probe -- --smoke --verify-operations --output report.json

#[cfg(windows)]
mod windows_probe {

    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use anyhow::{Context, Result, bail, ensure};
    use serde_json::{Value, json};
    use system_monitor_x::monitor::{Monitor, ProcessSample, SystemSnapshot};
    use system_monitor_x::platform::{self, Priority};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, GetPriorityClass, NORMAL_PRIORITY_CLASS,
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    #[derive(Default)]
    struct Options {
        smoke: bool,
        verify_operations: bool,
        output: Option<PathBuf>,
    }

    pub fn run() -> Result<()> {
        let mut arguments = std::env::args_os().skip(1).peekable();
        // The diagnostic parent only starts its own executable in this mode.
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--probe-owned-child")
        {
            arguments.next();
            ensure!(
                arguments.next().is_none(),
                "Owned-child mode accepts no additional arguments"
            );
            std::thread::sleep(Duration::from_secs(60));
            return Ok(());
        }
        let mut options = Options::default();
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--smoke") => options.smoke = true,
                Some("--verify-operations") => options.verify_operations = true,
                Some("--output") => {
                    let path = arguments.next().context("--output requires a file path")?;
                    ensure!(!path.is_empty(), "--output path must not be empty");
                    options.output = Some(path.into());
                }
                Some("--help" | "-h") => {
                    println!(
                        "monitor_probe [--smoke] [--verify-operations] [--output FILE]\n\nSamples real Windows telemetry twice, one second apart. Prints JSON to stdout.\n--smoke exits with an error when collector sanity checks fail.\n--verify-operations tests priority and termination only on its own child process.\n--output also saves the JSON report to FILE."
                    );
                    return Ok(());
                }
                _ => bail!("Unknown argument: {}", argument.to_string_lossy()),
            }
        }

        let mut monitor = Monitor::new();
        let first = monitor.sample();
        std::thread::sleep(Duration::from_secs(1));
        let second = monitor.sample();
        let checks = smoke_checks(&second);
        let smoke_passed = checks.iter().all(|check| check["passed"] == true);

        // Read failures are useful diagnostics, not privileged-environment failures.
        let services = match platform::list_services() {
            Ok(services) => json!({
                "available": true,
                "count": services.len(),
                "running_count": services.iter().filter(|service| service.status == "运行中").count(),
                "configuration_unavailable_count": services.iter().filter(|service| service.start_type == "无法读取配置").count(),
                "read_error": Value::Null,
            }),
            Err(error) => {
                json!({"available": false, "count": Value::Null, "read_error": format!("{error:#}")})
            }
        };
        let startup = match platform::list_startup() {
            Ok(entries) => {
                json!({"available": true, "count": entries.len(), "read_error": Value::Null})
            }
            Err(error) => {
                json!({"available": false, "count": Value::Null, "read_error": format!("{error:#}")})
            }
        };
        let elevation = match platform::is_elevated() {
            Ok(elevated) => json!({"elevated": elevated, "read_error": Value::Null}),
            Err(error) => json!({"elevated": Value::Null, "read_error": format!("{error:#}")}),
        };
        let operations = if options.verify_operations {
            match verify_operations(&mut monitor) {
                Ok(report) => report,
                Err(error) => {
                    json!({"requested": true, "passed": false, "error": format!("{error:#}")})
                }
            }
        } else {
            json!({"requested": false, "passed": Value::Null})
        };
        let operations_passed = !options.verify_operations || operations["passed"] == true;
        let mut report = snapshot_report(&second);
        report["application"] = json!("SystemMonitorX");
        report["version"] = json!(env!("CARGO_PKG_VERSION"));
        report["schema_version"] = json!(1);
        report["generated_at_unix_seconds"] =
            json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
        report["sampling"] = json!({
            "samples": 2,
            "requested_interval_ms": 1000,
            "observed_interval_seconds": second.sample_interval_secs,
            "first_cpu_ready": first.cpu_ready,
            "second_cpu_ready": second.cpu_ready,
        });
        report["services"] = services;
        report["startup"] = startup;
        report["privileges"] = elevation;
        report["checks"] =
            json!({"smoke_requested": options.smoke, "passed": smoke_passed, "results": checks});
        report["operations"] = operations;
        report["passed"] = json!(smoke_passed && operations_passed);

        let encoded = serde_json::to_string_pretty(&report)?;
        if let Some(path) = options.output {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Cannot create report directory {}", parent.display())
                })?;
            }
            std::fs::write(&path, format!("{encoded}\n"))
                .with_context(|| format!("Cannot write report {}", path.display()))?;
        }
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{encoded}")?;
        stdout.flush()?;
        ensure!(
            !options.smoke || smoke_passed,
            "Collector smoke checks failed; see the JSON report"
        );
        ensure!(
            operations_passed,
            "Owned-child operation checks failed; see the JSON report"
        );
        Ok(())
    }

    fn snapshot_report(snapshot: &SystemSnapshot) -> Value {
        let own = snapshot
            .processes
            .iter()
            .find(|process| process.pid == std::process::id());
        json!({
            "system": {
                "host_name": snapshot.host_name,
                "os_name": snapshot.os_name,
                "os_version": snapshot.os_version,
                "kernel_version": snapshot.kernel_version,
                "architecture": snapshot.architecture,
                "uptime_seconds": snapshot.uptime_secs,
                "boot_time_unix_seconds": snapshot.boot_time_unix_secs,
            },
            "cpu": {
                "name": snapshot.cpu_name.trim(),
                "usage_percent": snapshot.cpu_usage,
                "ready": snapshot.cpu_ready,
                "logical_cores": snapshot.logical_cpus,
                "physical_cores": snapshot.physical_cores,
                "frequency_mhz": snapshot.cpu_frequency_mhz,
                "cores": snapshot.cpus.iter().map(|cpu| json!({
                    "index": cpu.index, "name": cpu.name,
                    "usage_percent": cpu.usage_percent, "frequency_mhz": cpu.frequency_mhz,
                })).collect::<Vec<_>>(),
            },
            "memory": {
                "total_bytes": snapshot.memory_total,
                "used_bytes": snapshot.memory_used,
                "available_bytes": snapshot.memory_available,
                "usage_percent": snapshot.memory_percent(),
                "swap_total_bytes": snapshot.swap_total,
                "swap_used_bytes": snapshot.swap_used,
            },
            "gpu": {
                "count": snapshot.gpus.len(),
                "usage_percent": snapshot.gpu_percent(),
                "note": snapshot.gpu_note,
                "adapters": snapshot.gpus.iter().map(|gpu| json!({
                    "index": gpu.index, "name": gpu.name, "luid": format!("{:016x}", gpu.luid),
                    "usage_percent": gpu.usage_percent,
                    "dedicated_memory_used_bytes": gpu.dedicated_memory_used,
                    "dedicated_memory_total_bytes": gpu.dedicated_memory_total,
                    "shared_memory_used_bytes": gpu.shared_memory_used,
                    "shared_memory_total_bytes": gpu.shared_memory_total,
                    "temperature_celsius": gpu.temperature_celsius,
                    "status": gpu.status,
                    "engines": gpu.engines.iter().map(|engine| json!({
                        "name": engine.name, "usage_percent": engine.usage_percent,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            },
            "disks": {
                "count": snapshot.disks.len(),
                "read_bytes_per_second": snapshot.disk_read_bytes_per_sec,
                "write_bytes_per_second": snapshot.disk_write_bytes_per_sec,
                "active_percent": snapshot.disk_usage,
                "note": snapshot.io_note,
                "volumes": snapshot.disks.iter().map(|disk| json!({
                    "id": disk.id, "name": disk.name, "mount_point": disk.mount_point,
                    "kind": disk.kind, "file_system": disk.file_system,
                    "total_bytes": disk.total_bytes, "available_bytes": disk.available_bytes,
                    "read_bytes_per_second": disk.read_bytes_per_sec,
                    "write_bytes_per_second": disk.write_bytes_per_sec,
                    "total_read_bytes": disk.total_read_bytes, "total_write_bytes": disk.total_write_bytes,
                    "active_percent": disk.active_percent, "queue_depth": disk.queue_depth,
                    "read_latency_ms": disk.read_latency_ms, "write_latency_ms": disk.write_latency_ms,
                })).collect::<Vec<_>>(),
            },
            "network": {
                "count": snapshot.networks.len(),
                "receive_bytes_per_second": snapshot.network_receive_bytes_per_sec,
                "transmit_bytes_per_second": snapshot.network_transmit_bytes_per_sec,
                "note": snapshot.network_note,
                "interfaces": snapshot.networks.iter().map(|network| json!({
                    "name": network.name, "state": network.state,
                    "mac_address": network.mac_address, "ip_addresses": network.ip_addresses,
                    "received_bytes_per_second": network.received_bytes_per_sec,
                    "transmitted_bytes_per_second": network.transmitted_bytes_per_sec,
                    "total_received_bytes": network.total_received_bytes,
                    "total_transmitted_bytes": network.total_transmitted_bytes,
                })).collect::<Vec<_>>(),
            },
            "processes": {
                "count": snapshot.processes.len(),
                "gpu_usage_measured_count": snapshot.processes.iter().filter(|process| process.gpu_usage.is_some()).count(),
                "gpu_memory_measured_count": snapshot.processes.iter().filter(|process| process.gpu_memory_bytes.is_some()).count(),
                "own_process": own.map(process_report),
            },
            "temperatures": {
                "available": snapshot.temperatures.iter().any(|temperature| temperature.celsius.is_some())
                    || snapshot.gpus.iter().any(|gpu| gpu.temperature_celsius.is_some()),
                "system_sensor_count": snapshot.temperatures.iter().filter(|temperature| temperature.celsius.is_some()).count(),
                "gpu_sensor_count": snapshot.gpus.iter().filter(|gpu| gpu.temperature_celsius.is_some()).count(),
                "note": snapshot.temperature_note,
                "system_sensors": snapshot.temperatures.iter().map(|temperature| json!({
                    "label": temperature.label, "celsius": temperature.celsius,
                    "critical_celsius": temperature.critical_celsius,
                })).collect::<Vec<_>>(),
            },
        })
    }

    fn process_report(process: &ProcessSample) -> Value {
        json!({
            "pid": process.pid, "name": process.name,
            "parent_pid": process.parent_pid, "start_time_unix_seconds": process.start_time_unix_secs,
            "cpu_usage_percent": process.cpu_usage, "memory_bytes": process.memory_bytes,
            "disk_read_bytes_per_second": process.disk_read_bytes_per_sec,
            "disk_write_bytes_per_second": process.disk_write_bytes_per_sec,
            "gpu_usage_percent": process.gpu_usage, "gpu_memory_bytes": process.gpu_memory_bytes,
        })
    }

    fn valid_percentage(value: f32) -> bool {
        value.is_finite() && (-0.01..=100.01).contains(&value)
    }

    fn valid_rate(value: Option<f64>) -> bool {
        value.is_none_or(|value| value.is_finite() && value >= 0.0)
    }

    fn smoke_checks(snapshot: &SystemSnapshot) -> Vec<Value> {
        let own = snapshot
            .processes
            .iter()
            .find(|process| process.pid == std::process::id());
        let mut results = Vec::new();
        let mut check = |name: &str, passed: bool, detail: &str| {
            results.push(json!({"name": name, "passed": passed, "detail": detail}));
        };
        check(
            "cpu_ready_after_second_sample",
            snapshot.cpu_ready,
            "Two actual samples are separated by at least one second",
        );
        check(
            "cpu_range",
            valid_percentage(snapshot.cpu_usage)
                && snapshot
                    .cpus
                    .iter()
                    .all(|cpu| valid_percentage(cpu.usage_percent))
                && snapshot
                    .processes
                    .iter()
                    .all(|process| valid_percentage(process.cpu_usage)),
            "System, core and machine-normalized process CPU percentages are finite and within 0..100",
        );
        check(
            "cpu_enumeration",
            snapshot.logical_cpus > 0 && snapshot.logical_cpus == snapshot.cpus.len(),
            "Logical CPU count matches collected cores",
        );
        check(
            "memory_bounds",
            snapshot.memory_total > 0
                && snapshot.memory_used <= snapshot.memory_total
                && snapshot.memory_available <= snapshot.memory_total,
            "Memory totals are real and internally consistent",
        );
        check(
            "own_process_visible",
            own.is_some(),
            "The diagnostic executable appears in the process snapshot",
        );
        check(
            "own_process_io_available",
            own.is_some_and(|process| {
                process.disk_read_bytes_per_sec.is_some()
                    && process.disk_write_bytes_per_sec.is_some()
            }),
            "Native I/O successfully read the owned process; genuine idle values may be zero",
        );
        check(
            "rate_bounds",
            valid_rate(snapshot.disk_read_bytes_per_sec)
                && valid_rate(snapshot.disk_write_bytes_per_sec)
                && valid_rate(snapshot.network_receive_bytes_per_sec)
                && valid_rate(snapshot.network_transmit_bytes_per_sec)
                && snapshot.disks.iter().all(|disk| {
                    valid_rate(disk.read_bytes_per_sec) && valid_rate(disk.write_bytes_per_sec)
                })
                && snapshot.networks.iter().all(|network| {
                    valid_rate(network.received_bytes_per_sec)
                        && valid_rate(network.transmitted_bytes_per_sec)
                })
                && snapshot.processes.iter().all(|process| {
                    valid_rate(process.disk_read_bytes_per_sec)
                        && valid_rate(process.disk_write_bytes_per_sec)
                }),
            "Available rates are finite and nonnegative; absent providers remain null",
        );
        check(
            "gpu_range",
            snapshot.gpus.iter().all(|gpu| {
                gpu.usage_percent.is_none_or(valid_percentage)
                    && gpu
                        .engines
                        .iter()
                        .all(|engine| valid_percentage(engine.usage_percent))
            }),
            "Available GPU and engine percentages are finite and within 0..100; headless hosts may have no GPU",
        );
        check(
            "disk_range",
            snapshot
                .disks
                .iter()
                .all(|disk| disk.active_percent.is_none_or(valid_percentage)),
            "Available volume activity percentages are finite and within 0..100",
        );
        results
    }

    struct OwnedChild(Child);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // std::process::Child retains the handle of this exact test child.
            // Cleanup never looks up another process by PID and runs on failures too.
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    fn child_priority(pid: u32) -> Result<u32> {
        // SAFETY: read-only access to the child owned by this diagnostic process.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
        let priority = unsafe { GetPriorityClass(handle) };
        let _ = unsafe { CloseHandle(handle) };
        ensure!(
            priority != 0,
            "Cannot read the owned child's priority class"
        );
        Ok(priority)
    }

    fn verify_operations(monitor: &mut Monitor) -> Result<Value> {
        let executable = std::env::current_exe()?;
        let mut child = OwnedChild(
            Command::new(executable)
                .arg("--probe-owned-child")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn()
                .context("Cannot launch the diagnostic's owned child")?,
        );
        let pid = child.0.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        let identity = loop {
            ensure!(
                child.0.try_wait()?.is_none(),
                "Owned child exited before identity verification"
            );
            if let Some(process) = monitor.sample().processes.into_iter().find(|process| {
                process.pid == pid
                    && process.start_time_unix_secs > 0
                    && process.parent_pid == Some(std::process::id())
            }) {
                break process;
            }
            ensure!(
                Instant::now() < deadline,
                "Owned child did not appear in the process collector"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let start_time = identity.start_time_unix_secs;
        let invalid_start = start_time
            .checked_add(60)
            .context("Invalid test start time")?;
        let invalid_priority = platform::set_priority(pid, invalid_start, Priority::BelowNormal);
        ensure!(
            invalid_priority.is_err(),
            "Priority operation accepted a mismatched process identity"
        );
        let invalid_termination = platform::terminate_process(pid, invalid_start);
        ensure!(
            invalid_termination.is_err(),
            "Termination accepted a mismatched process identity"
        );
        let missing_identity = platform::terminate_process(pid, 0);
        ensure!(
            missing_identity.is_err(),
            "Termination accepted a missing process identity"
        );
        ensure!(
            child.0.try_wait()?.is_none(),
            "An invalid identity operation terminated the owned child"
        );

        platform::set_priority(pid, start_time, Priority::BelowNormal)
            .context("Cannot lower owned-child priority")?;
        ensure!(
            child_priority(pid)? == BELOW_NORMAL_PRIORITY_CLASS.0,
            "Below-normal priority was not applied"
        );
        platform::set_priority(pid, start_time, Priority::Normal)
            .context("Cannot restore owned-child priority")?;
        ensure!(
            child_priority(pid)? == NORMAL_PRIORITY_CLASS.0,
            "Normal priority was not restored"
        );
        platform::terminate_process(pid, start_time)
            .context("Cannot terminate the owned test child")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let exit_status = loop {
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            ensure!(
                Instant::now() < deadline,
                "Owned-child termination did not complete within five seconds"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        ensure!(
            platform::launch_task("").is_err(),
            "Empty task launch was accepted"
        );
        Ok(json!({
            "requested": true, "passed": true,
            "owned_child_pid": pid, "owned_child_start_time_unix_seconds": start_time,
            "wrong_start_time_priority_rejected": true,
            "wrong_start_time_termination_rejected": true,
            "missing_start_time_termination_rejected": true,
            "below_normal_priority_verified": true,
            "normal_priority_restored": true,
            "owned_child_terminated": true,
            "exit_code": exit_status.code(),
            "empty_task_launch_rejected": true,
            "scope": "Only a child created by this diagnostic executable was modified",
        }))
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_probe::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("monitor_probe requires Windows");
    std::process::exit(1);
}

# SystemMonitorX

English | [简体中文](README.zh-CN.md)

A native Windows 10/11 system monitor and process manager built with **Rust 2024 and Longbridge GPUI Kit 0.7.1**. [Public repository](https://github.com/neko233-com/SystemMonitorX).

Version 0.3 adds stable name sorting, real executable icons, developer process/port workflows, application grouping, native CPU topology, power information, offline signature inspection, and a PowerShell installer. **The UI currently uses Chinese labels.** Process names, file paths and device identities retain the original Windows data. See the [roadmap](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.md) for remaining work.

## Download and install

Download from [Releases](https://github.com/neko233-com/SystemMonitorX/releases):

- **SystemMonitorX-0.3.0-x64.msi**: per-user installation, upgrade and uninstall, with a Start menu shortcut.
- **SystemMonitorX-0.3.0-x64-portable.zip**: extract the complete archive, then run SystemMonitorX.exe.
- **SHA256SUMS.txt**: SHA-256 checksums for both packages.

Requires Windows 10 1803 (build 17134) or later / Windows 11, x64, and a GPU/driver supported by GPUI's Direct3D renderer. Default directory: `%LOCALAPPDATA%\Programs\SystemMonitorX`. Keep SystemMonitorXUpdate.exe and all bundled VC runtime DLLs beside the main executable.

One-command install or upgrade in Windows PowerShell 5.1 or PowerShell 7:

~~~powershell
& ([ScriptBlock]::Create((irm 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1'))) -Mirror Auto
~~~

[Installation guide](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/installation.md): inspect/save the script first, select a version, download only, extract a portable installation, or supply a per-invocation loopback proxy. GitHub is tried first, then bounded mirror fallbacks; metadata and SHA-256 always come from official project URLs. No global execution policy, proxy, certificate or firewall changes.

The application runs with normal user permissions. Elevation is explicit and Windows handles consent. It does not modify the built-in Task Manager registration or system shortcuts. Packages are currently unsigned.

Public v0.3.0 was installed through the PS5.1 script (MSI exit 0, no restart). All 16 installed files match the public ZIP; both executable versions, HKCU location and Start menu target passed. A portable copy started and closed normally from a path containing Chinese, accented Latin and Japanese characters.

## Developer workflows

Lists default to the same Unicode lowercase name ordering, with PID and birth identity breaking ties. Metric refreshes reuse name order; changing CPU usage does not continuously move rows. Click another column when you explicitly want numeric sorting.

| Task | Workflow |
| --- | --- |
| Find a process | Plain text, `pid:1234`, `ppid:1234`, `name:chrome`, `path:projects` |
| Find a port owner | `port:8080`, `pid:1234`, `tcp listen`, `udp v6`; combine terms with spaces |
| Process context menu | Query ports, copy PID/path/command line, open file location, confirm process/tree/application termination |
| Port context menu | Copy PID/endpoints/path, jump to the captured process identity, confirm termination |
| Export | Ctrl+E exports the current filtered page; port CSV includes protocol, address family, endpoints, state, PID and identity notes |

Icons load for visible rows on one background worker with bounded queues and a TTL cache. Ports refresh every two seconds while that page is open. Hidden pages defer display-model work. There is no WebView or browser runtime.

## Monitoring and management

| Page | Capabilities |
| --- | --- |
| Processes / Details | Virtual tables, PID, CPU, working set, GPU, VRAM, I/O and process network; stable sorting, application groups, process trees, optional columns; priority, current-group affinity, efficiency mode and termination |
| Selected process | Asynchronous command line, architecture/WOW64 and executable version resources; on-demand offline embedded/catalog Authenticode verification |
| Performance | CPU, memory, multiple GPUs, disks and network; 120-sample history, logical CPU loads, process/thread/handle counts, commit/cache/kernel memory |
| Devices | CPU groups/core/NUMA/cache/efficiency classes, power/battery, motherboard/BIOS, DIMMs, storage, NICs and GPU driver/PCI identity |
| DirectX 12 | Per-adapter feature level, shader model, ray tracing, mesh shader, sampler feedback, enhanced barriers and node count |
| Services | SCM status/configuration and confirmed background start/stop/restart |
| Startup | HKCU/HKLM Run in both views, user/common Startup folders, StartupApproved state and enable/disable |
| Users | WTS sessions and resource aggregation; confirmed disconnect/logoff |
| History | Observed CPU/I/O and available ETW bytes, ended-process retention, application aggregation and bounded persistence |
| Settings | 0.5 / 1 / 2 second sampling, theme, always-on-top, columns, elevation, automatic/manual update checks and one-click installation |

Management verifies PID and original creation FILETIME, then acts on the same native handle. Critical processes and this application are protected. Application termination validates every captured member before acting. Expensive collection, hardware, file and management operations run in the background with immutable snapshots.

## Data sources and limits

- **CPU/processes:** native NtQuerySystemInformation counters and actual sample intervals, with fallback/error reporting. CPU percentages use the whole machine as 100%; insufficient samples remain unavailable.
- **Memory/I/O:** working set and private commit are distinct; shared pages can appear in multiple processes. Win32 supplies system commit/cache/kernel data; IOCTL_DISK_PERFORMANCE supplies volume I/O. Units use powers of 1024.
- **GPU:** DXGI/D3DKMT identity and WDDM/PDH engine/memory counters; supported NVIDIA devices add NVML sensors. AMD/Intel vendor sensors and general CPU-temperature coverage remain incomplete. The [Windows process-GPU memory issue](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value) is detected when a process count exceeds known positive adapter capacity; other errors may remain.
- **Application groups:** real window and parent/birth identities. GPU utilization currently shows the maximum member value, not merged group engine utilization.
- **Ports:** IP Helper owner-PID tables for TCP/UDP IPv4/IPv6. Endpoint and process reads are not atomic. Uncertain identities retain a reason and cannot be terminated through that row. UDP has no fabricated remote endpoint or TCP state.
- **Process network:** Kernel-Network ETW/TDH event bytes associated with exact process birth. Administrator/kernel tracing permission is usually required. Permission, dropped-event and schema failures display unavailable status; a port list is not a traffic counter.
- **History:** starts from observation baselines. Processes entirely between samples and time when this application was not running cannot be reconstructed.
- **Hardware/signatures:** cached WMI/Win32 data with manual refresh. Offline WinVerifyTrust uses local embedded/catalog evidence without online revocation. Version-resource CompanyName is separate from the verified signer.
- **DirectX:** the UI renders with **D3D11**; querying D3D12 capabilities does not switch its renderer.

Unavailable values remain unavailable. Full coverage of all hardware, complete Task Manager parity, and comprehensive performance superiority have not been established. Cross-group affinity, additional startup sources, remote current-directory inspection, localization and accessibility remain roadmap work.

## Updates

While running, the application checks this project's stable release every six hours by default, retries failures, and offers manual checks or disabling automatic checks. Automatic checking does not automatically install.

The downloader verifies repository URL, stable version, filename, size and SHA-256. A detached helper locks and verifies the MSI again, acquires an installation mutex, waits for the exact old process to exit, and invokes Windows Installer without automatic reboot. Success starts the installed application; cancellation/failure attempts to reopen the original copy. A portable copy's one-click update installs the per-user version.

Logs and last-install-result.json are under `%LOCALAPPDATA%\SystemMonitorX\updates`. A missing helper or incomplete bundle leaves the current window open with an error.

Version 0.2.1 fixed older extended-path MSI handoff failures. Existing 0.2.0 users should install a current MSI once to obtain the fixed helper. The repaired helper's real Ready/download/verify/install/relaunch path and public 0.2.0 → 0.2.1 upgrade were verified. The complete automatically discovered newer-version branch and cancellation/failure recovery still need broader regression.

## Shortcuts and international use

| Shortcut | Action |
| --- | --- |
| Ctrl+F | Search |
| Ctrl+N | Run a new task |
| Ctrl+E | Export the current page |
| Ctrl+P | Pause/resume display; collection continues |

Settings/history are in `%LOCALAPPDATA%\SystemMonitorX`. Update checks do not upload device or process telemetry. Windows paths use UTF-16 APIs, counter APIs are locale independent, name keys are Unicode, and units are explicit. English/Chinese documentation is provided; full UI language switching and locale presentation still need implementation.

## Build and verify

Requires Rust 1.96.0, Visual Studio Desktop development with C++, Windows SDK, and .NET SDK / WiX 5.0.2 for packaging. Cargo.lock pins dependencies.

~~~powershell
. ./scripts/Enter-DevEnvironment.ps1
cargo run --bin SystemMonitorX
./scripts/Verify.ps1
./scripts/Package.ps1
~~~

Verification covers formatting, strict Clippy, tests, installer self-tests and live telemetry. Mutation tests act only on dedicated test children. Packaging validates MSI properties, per-user directories, shortcut targets and extracted payload hashes. Tags trigger Windows builds/releases; results are in [Actions](https://github.com/neko233-com/SystemMonitorX/actions).

Optional real Windows GPUI/D3D11 UI regression:

~~~powershell
cargo run --release --features ui-test --bin SystemMonitorX -- --ui-smoke-dir C:/Temp/SystemMonitorX-ui-check
~~~

154 source tests pass, with two environment-dependent tests ignored. PowerShell 5.1/7 each pass 29 installer checks. The v0.3 optimized native UI build passes 33 checks including executable icons, both context menus and exact port-to-process navigation.

With 994 real processes, 230 directly measured key presses had CPU-side p50 **4.19 ms**, p95 **5.85 ms**, and max **10.54 ms**; 30 explicit render phases had p95 **5.33 ms**. These include test instrumentation, exclude PNG encoding and are not GPU presentation latency or an official Task Manager comparison. A separate synthetic 10,000-row model is never injected into live telemetry. Reports/screenshots can contain local process/device information and should stay private. See the [roadmap](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.md) for package/install evidence and remaining regression.

## Technology

[Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit), [sysinfo](https://docs.rs/sysinfo/0.39.6/sysinfo/), [Rust for Windows](https://github.com/microsoft/windows-rs), [Microsoft GPU statistics](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/), [NVIDIA NVML](https://docs.nvidia.com/deploy/nvml-api/index.html), [Windows Installer](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/msiexec), [WiX](https://docs.firegiant.com/wix/).

Apache-2.0. See [LICENSE](LICENSE).

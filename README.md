# SystemMonitorX

English | [简体中文](README.zh-CN.md)

**See which apps use your PC’s resources, find a process or port owner, and manage it in one native window.**

[Download for Windows (MSI)](https://github.com/neko233-com/SystemMonitorX/releases/download/v0.5.0/SystemMonitorX-0.5.0-x64.msi) · [Portable ZIP](https://github.com/neko233-com/SystemMonitorX/releases/download/v0.5.0/SystemMonitorX-0.5.0-x64-portable.zip) · [简体中文](README.zh-CN.md)

| What you need | Start here |
| --- | --- |
| Check which apps are busy | Open **Overview**, then click a top CPU or memory process |
| Find an app or PID | Open **Processes**, type a name or `pid:1234` |
| Find who uses a busy port | Open **Ports**, type `port:3000`, then right-click to view its process |
| Understand an unfamiliar process | Right-click **Open file location**, or open **Process tools** for its modules |

![Overview: resource usage and the main processes](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/overview-en.png)

*Anonymous illustrative data rendered by the real native Windows window. These examples are not live telemetry or performance benchmarks.*

A native Windows 10/11 system monitor and process manager built with **Rust 2024 and Longbridge GPUI Kit 0.7.3**. [Public repository](https://github.com/neko233-com/SystemMonitorX).

**v0.5.0 adds a simple Overview, five-column process view, saved searches, quick actions, and on-demand developer diagnostics.** Start with resource usage and the main consumers; open detailed tools only when needed. Eight UI languages, stable name sorting, real process icons/PID, CPU/GPU/hardware information and verified update downloads remain available. See the [roadmap](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.md) for acceptance status and remaining work.

## Download and install

Just want to use it? **Download the MSI → open it → launch SystemMonitorX from Start.** Begin with Overview. Check and install updates in **Settings**. For a portable copy, extract the entire ZIP and open `SystemMonitorX.exe`.

Choose a published version from [Releases](https://github.com/neko233-com/SystemMonitorX/releases). The v0.5.0 package names are:

- **SystemMonitorX-0.5.0-x64.msi**: per-user installation, upgrade and uninstall, with a Start menu shortcut.
- **SystemMonitorX-0.5.0-x64-portable.zip**: extract the complete archive, then run `SystemMonitorX.exe`.
- **SHA256SUMS.txt**: SHA-256 checksums for both packages.

Requires Windows 10 1803 (build 17134) or later / Windows 11, x64, and a GPU/driver supported by GPUI's Direct3D renderer. Default directory: `%LOCALAPPDATA%\Programs\SystemMonitorX`. Keep `SystemMonitorXUpdate.exe` and all bundled VC runtime DLLs beside the main executable.

One-command install or upgrade in Windows PowerShell 5.1 or PowerShell 7:

~~~powershell
& ([ScriptBlock]::Create((irm 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1'))) -Mirror Auto
~~~

The command selects the latest **published stable release**. A specified version must exist in Releases. See the [installation guide](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/installation.md) to inspect/save the script first, select a version, download only, extract a portable installation, or supply a per-invocation loopback proxy. GitHub is tried first, then bounded mirror fallbacks; metadata and SHA-256 always come from official project URLs. The script does not change global execution policy, proxy settings, certificate trust or the firewall.

The application runs with normal user permissions. Elevation is explicit and Windows handles consent. It does not modify the built-in Task Manager registration or system shortcuts. Packages are currently unsigned.


## Start here

1. Open **Overview** to see CPU, memory, GPU and disk usage with their main observed consumers. These are snapshot summaries, not a diagnosis of computer health.
2. Open **Processes** to find an application by name. A fresh setup shows **Name, PID, CPU, Memory and GPU**; use **Columns → Common columns / All columns** for the level of detail you need. Existing saved column choices are preserved.
3. Press **F1** for in-app help or **Ctrl+Shift+P** to find a page or action. Use **F5** to refresh the current page. A missing measurement appears as **—**, rather than a guessed value.

## Screenshots

**Processes:** the default view shows Name, PID, CPU, Memory and GPU. Names stay in A–Z order while measurements refresh.

![Processes: icons, PID and five common columns](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/processes-en.png)

<details>
<summary>Investigating a port or a process? Open these two examples</summary>

**Ports:** type `port:3000` to find an owner, or `pid:1234` for a process’s endpoints. Right-click to view the process or request confirmed termination.

![Ports: TCP/UDP endpoints and their process owners](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/ports-en.png)

**Process tools:** right-click a process to inspect loaded modules and virtual-memory metadata, copy a query, or save diagnostic JSON. Copying a query does not run it.

![Process tools: on-demand inspection and diagnostic export](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/process-tools-en.png)

These are anonymous illustrative samples. See the [developer guide](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/developer-tools.md) for details.

</details>

## Languages and international use

| Language | Saved locale |
| --- | --- |
| 简体中文 | `zh-CN` |
| English | `en-US` |
| 日本語 | `ja-JP` |
| 한국어 | `ko-KR` |
| Deutsch | `de-DE` |
| Français | `fr-FR` |
| Español | `es-ES` |
| Português (Brasil) | `pt-BR` |

The default **Follow system** preference resolves Windows UI-language preferences to a supported language, with English as the fallback. Select a language in **Settings → Interface language** to switch immediately and save the preference for the next launch.

Language switching translates application labels, dialogs, cached feedback and available hardware presentation. Process/device names, paths, PID/creation identities, account names and raw Windows diagnostics retain their source values. It preserves the current search, stable selection, sorting and column widths. Cached hardware is reformatted in the background without another WMI/device query; generation checks prevent an older-language result replacing the newer presentation.

Search hints and syntax depend on the current page; typing filters that page's model. Navigating to another page clears the query. **CSV headers remain English** for stable tooling. Names use a consistent Unicode lowercase ordering, not language-specific collation; current numeric/unit formatting is not a complete regional-formatting implementation. The application's saved language preference controls its UI only. The MSI's own product and installation-condition text is English. Windows Installer system messages, UAC and native OS dialogs use Windows' own language settings. Complete screen-reader and keyboard accessibility remain roadmap work.

## Developer workflows

Lists default to the same Unicode lowercase name ordering, with PID and birth identity breaking ties. Metric refreshes reuse name order; changing CPU usage does not continuously move rows. Click another column when you explicitly want numeric sorting.

| Task | Workflow |
| --- | --- |
| Find a process | Plain text, `pid:1234`, `ppid:1234`, `name:chrome`, `path:projects` |
| Find a port owner | `port:8080`, `pid:1234`, `tcp listen`, `udp v6`; combine terms with spaces |
| Process context menu | Query ports, copy PID/path/command line, open file location, confirm process/tree/application termination |
| Port context menu | Copy PID/endpoints/path, jump to the captured process identity, confirm termination |
| Saved searches | Save/reuse/delete up to eight process and port queries in total; each is shown on its corresponding page |
| Process tools | Read-only DLL/module and virtual-address diagnostics, identity-checked PowerShell queries, explicit terminal launch, copy/save diagnostic JSON |
| Export | Ctrl+E exports search-matching processes on Processes/Details, filtered endpoints on Ports, and the complete process snapshot on other pages; headers stay English |

Process/Details export includes all raw processes matching the search, including members hidden by a collapsed group or tree. Process CSV includes the captured raw identity and available CPU, memory, GPU, I/O, network, thread/handle and account fields. Missing optional fields stay empty. Port CSV includes protocol, address family, endpoints, state, PID, executable path, creation FILETIME, query and identity notes. CSV quoting and formula-injection protection retain literal source data.

Diagnostic JSON captures only the selected process and matching PID/creation FILETIME endpoints. It has fixed English keys, a schema version, collection timestamps/sources and null unavailable fields. Command lines and environment variables are excluded by default; paths, account names and network addresses can still identify local data. Windows creation ticks are decimal strings to preserve exact identity in JSON consumers. Copying a PowerShell query does not run it. **Open PowerShell in program folder** opens a terminal only after that action is clicked, in the executable folder rather than an assumed remote working directory. See the bilingual [developer guide](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/developer-tools.md) for diagnostics and launch arguments.

Icons load for visible rows on one background worker with bounded queues and a TTL cache. Ports refresh every two seconds while that page is open. Hidden pages defer display-model work. There is no WebView or browser runtime.

## Monitoring and management

| Page | Capabilities |
| --- | --- |
| Overview | Snapshot CPU/memory/GPU/disk summaries, main observed resource consumers, capacity hints and links to detailed pages |
| Processes / Details | Virtual tables, PID, CPU, working set, GPU, VRAM, I/O and process network; stable sorting, application groups, process trees, optional columns; priority, current-group affinity, efficiency mode and termination |
| Selected process | Asynchronous command line, architecture/WOW64 and executable version resources; offline embedded/catalog Authenticode inspection; on-demand read-only DLL and virtual-address diagnostics with JSON export |
| Performance | CPU, memory, multiple GPUs, disks and network; 120-sample history, logical CPU loads, process/thread/handle counts, commit/cache/kernel memory |
| Devices | CPU groups/core/NUMA/cache/efficiency classes, power/battery, motherboard/BIOS, DIMMs, storage, NICs and GPU driver/PCI identity |
| DirectX 12 | Per-adapter feature level, shader model, ray tracing, mesh shader, sampler feedback, enhanced barriers and node count |
| Services | SCM status/configuration and confirmed background start/stop/restart |
| Startup | HKCU native-view Run, HKLM 32-/64-bit Run, user/common Startup folders, StartupApproved state and enable/disable with the original command retained |
| Users | WTS sessions and resource aggregation; confirmed disconnect/logoff |
| History | Observed CPU/I/O and available ETW bytes, ended-process retention, application aggregation and bounded persistence |
| Settings | Eight languages/System preference, 0.5 / 1 / 2 / 5 second sampling, theme, always-on-top, columns, elevation, automatic/manual update checks and one-click installation |

Management verifies PID and original creation FILETIME, then acts on the same native handle. Critical processes and this application are protected. Application termination validates every captured member before acting. SCM state and startup selection use raw enums/identities rather than translated text, so switching languages does not change the management target. Expensive collection, hardware, file and management operations run in the background with immutable snapshots.

## Data sources and limits

- **CPU/processes:** native `NtQuerySystemInformation` counters and actual sample intervals, with fallback/error reporting. CPU percentages use the whole machine as 100%; insufficient samples remain unavailable.
- **Memory/I/O:** working set and private commit are distinct; shared pages can appear in multiple processes. Win32 supplies system commit/cache/kernel data; `IOCTL_DISK_PERFORMANCE` supplies volume I/O. Process I/O includes file/device/other operations, not just network traffic. Units use powers of 1024.
- **GPU:** DXGI/D3DKMT identity and WDDM/PDH engine/memory counters; supported NVIDIA devices add NVML sensors. AMD/Intel vendor sensors and general CPU-temperature coverage remain incomplete. The [Windows process-GPU memory issue](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value) is detected when reported dedicated process GPU memory exceeds known positive adapter capacity; other errors may remain. Adapter-memory counters represent the whole adapter, not this application's DXGI memory budget.
- **Application groups:** real window and parent/birth identities. GPU utilization currently shows the maximum member value, not merged group engine utilization. Application classification can differ from Windows.
- **Ports:** IP Helper owner-PID tables for TCP/UDP IPv4/IPv6. Endpoint and process reads are not atomic. Uncertain identities retain a reason and cannot be terminated through that row. UDP has no fabricated remote endpoint or TCP state. Endpoint lists are not traffic counters.
- **Process network:** Kernel-Network ETW/TDH event bytes associated with exact process birth. Administrator/kernel tracing permission is usually required. Permission, dropped-event and schema failures display unavailable status. Owned-loopback aggregate bytes and trace cleanup have historical verification; correct owned bytes per protocol, long-running external traffic and broader schemas still need regression.
- **History:** starts from observation baselines. Processes entirely between samples and time when this application was not running cannot be reconstructed. Actual network bytes are accumulated only across valid ETW periods; coverage gaps are retained and storage is bounded.
- **Hardware/signatures:** cached WMI/Win32 data with manual refresh. Offline WinVerifyTrust uses local embedded/catalog evidence without online revocation. Version-resource CompanyName is separate from the verified signer; absent signing timestamps stay unknown.
- **Process diagnostics:** Toolhelp module snapshots and VirtualQueryEx region metadata are collected only on demand, on a worker, with exact identity checks. Default limits are 4,096 modules, 16,384 regions and a two-second cooperative budget checked between Windows calls; a single native call can exceed the budget. No target memory contents are read and the process is not suspended or modified. Virtual address size/reservation/commit metadata is not resident RAM or a memory-leak diagnosis. Partial/unavailable results retain their reason; access permissions, bitness and changing module lists can limit coverage.
- **Management:** affinity is limited to the current processor group. Additional scheduled-task, packaged-application and other logon-startup sources, measured startup impact, and per-user process expansion remain incomplete.
- **DirectX:** the UI renders with **D3D11**; querying D3D12 capabilities does not switch its renderer. Stable remote-process current-directory inspection is not implemented.

Unavailable optional measurements remain unavailable. Full coverage of all hardware, complete Task Manager parity, and comprehensive performance superiority have not been established.

## Updates

While running, the application checks this project's stable release every six hours by default, retries failures, and offers manual checks or disabling automatic checks. Automatic checking does not automatically install.

The downloader verifies repository URL, stable version, filename, size and SHA-256. A detached helper locks and verifies the MSI again, acquires an installation mutex, waits for the exact old process to exit, and invokes Windows Installer without automatic reboot. Success starts the installed application; cancellation/failure attempts to reopen the original copy. A portable copy's one-click update installs the per-user version.

Logs and `last-install-result.json` are under `%LOCALAPPDATA%\SystemMonitorX\updates`. A missing helper or incomplete bundle leaves the current window open with an error.

Version 0.2.1 fixed older extended-path MSI handoff failures. Existing 0.2.0 users should install a current MSI once to obtain the fixed helper. The repaired helper's real Ready/download/verify/install/relaunch path and public 0.2.0 → 0.2.1 upgrade were verified. The complete automatically discovered newer-version branch and cancellation/failure recovery still need broader regression.

## Shortcuts and local data

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+P | Find a page or action |
| F1 | Open in-app help |
| F5 | Refresh the current page; telemetry views use the latest collected snapshot |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Overview / Processes / Ports |
| Ctrl+F | Search the current searchable page |
| Ctrl+N | Run a new task |
| Ctrl+E | Export according to the current page's scope |
| Ctrl+P | Pause/resume display; collection continues |

Settings/history are in `%LOCALAPPDATA%\SystemMonitorX`. Update checks do not upload device or process telemetry. Windows paths use UTF-16 APIs, counter APIs are locale independent, and names retain Unicode source data. English and complete Chinese documentation are linked throughout.

## Build and verify

**v0.5.0 source/native validation:** 238 source tests passed (178 library + 60 GUI); 2 environment-dependent tests remain ignored. All 54 real Windows/D3D11 interaction checks passed, including dedicated minimum-window and eight-language acceptance at 1040×640 logical pixels and 1.5× scale. Ten self-process module/memory checks and 29 installer self-tests passed. Public package and installation acceptance is maintained in the [online roadmap](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.md).

Requires Rust 1.96.0, Visual Studio Desktop development with C++, Windows SDK, Python 3 for the standard-library translation validation gate, and .NET SDK / WiX 5.0.2 for packaging. Python is a development-verification dependency and is not bundled with the application. `Cargo.lock` pins dependencies.

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

**Historical v0.4.0 verification:** source tests: **187 passed, 2 ignored**. The PowerShell installer has 29 offline self-tests previously run under PS5.1 and PS7. Native Windows/D3D11 QA passed **34/34 checks**, with **8 Settings language switches and 40 screenshots at 1040×640 logical pixels, 1.5× scale**. Manual review covered Processes and Settings in all eight languages (**16/40 screenshots**); the remaining 24 were covered by automated native checks. Actual package/install acceptance for that version is maintained in the [online roadmap](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.md). Reports/screenshots can contain local process/device information and should stay private.

The final v0.4.0 native run observed **806 real processes**. Across **230 individual keystrokes**, keyboard dispatch/filter CPU time was p50 **3.6232 ms**, p95 **5.6238 ms**, and max **9.5156 ms**. Across **30 renders**, `render_frame` CPU time was p95 **6.2899 ms** and max **6.7198 ms**. These timings include test-observation overhead, exclude PNG encoding and GPU presentation, and are not an official Task Manager comparison. A separate synthetic 10,000-row model is never injected into live telemetry. Detailed native scope and historical results are recorded in [ROADMAP.md](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.md).

## Technology

[Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit), [sysinfo](https://docs.rs/sysinfo/0.39.6/sysinfo/), [Rust for Windows](https://github.com/microsoft/windows-rs), [Microsoft GPU statistics](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/), [NVIDIA NVML](https://docs.nvidia.com/deploy/nvml-api/index.html), [Windows Installer](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/msiexec), [WiX](https://docs.firegiant.com/wix/).

Apache-2.0. See [LICENSE](LICENSE).

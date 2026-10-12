# SystemMonitorX

[English](README.md) | 简体中文

面向 Windows 10/11 的原生系统监控与进程管理器，使用 **Rust 2024 + Longbridge GPUI Kit 0.7.1**。公开仓库：[neko233-com/SystemMonitorX](https://github.com/neko233-com/SystemMonitorX)。

v0.3 增加默认名称 A–Z 稳定排序、真实 EXE 图标、PID 查询与右键菜单、TCP/UDP 端口占用、应用分组、CPU 原生拓扑、供电信息、离线文件签名和 PowerShell 一键安装。GPUI 使用原生 Windows 窗口，界面当前由 **Direct3D 11** 渲染；DirectX 12 页面查询各显卡的实际能力。完整任务管理器替代目标、待实现项和验证进度见 [ROADMAP.md](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)。

## 下载与安装

在 [Releases](https://github.com/neko233-com/SystemMonitorX/releases) 选择实际已发布的版本。v0.3.0 提供 MSI、完整便携 ZIP、SHA-256 校验值和安装脚本：

- `SystemMonitorX-0.3.0-x64.msi`：当前用户安装，默认目录为 `%LOCALAPPDATA%\Programs\SystemMonitorX`，创建开始菜单快捷方式，支持升级和卸载。
- `SystemMonitorX-0.3.0-x64-portable.zip`：完整解压后运行 `SystemMonitorX.exe`。
- `SHA256SUMS.txt`：MSI 和 ZIP 的 SHA-256 校验值。

支持 Windows 10 1803（Build 17134）及以上 / Windows 11 x64，需要支持 GPUI Direct3D 渲染的显卡和驱动。应用默认按普通用户权限运行；管理员入口由用户主动选择，Windows 负责权限确认。应用不会修改默认任务管理器或系统快捷键。MSI 使用每用户安装范围，实际可用权限仍由 Windows 策略决定；发布包目前未使用商业证书签名。

请保留完整包中的 `SystemMonitorXUpdate.exe` 与随包 VC 运行库 DLL。只复制主 EXE、缺少更新助手的开发目录或不完整的便携包无法完成一键升级。

公开 v0.3.0 已通过 PS5.1 脚本从 MSI 升级本机（返回 0、无需重启）。安装版全部 16 个文件匹配公开 ZIP；主程序/助手版本、HKCU 目录和开始菜单目标核验通过。包含中文、重音字符和日文路径的便携版已启动并正常关闭。

## 面向开发者

所有列表默认按相同的名称 A–Z 规则排列，同名进程按 PID 与出生身份稳定排序。指标刷新复用名称顺序；只有身份或名称变化才重建顺序。仍可主动点击列标题按 CPU、内存等排序。进程页可按实际窗口和父子身份分组，详细信息页保留独立 PID；分组展开和进程树也使用一致的名称顺序。

- 进程搜索：pid:1234 精确 PID、ppid:1234 父 PID、name:chrome 名称前缀、path:projects 路径子串。
- 端口搜索：port:8080、pid:1234、tcp listen、udp v6；多个条件以空格组合。
- 进程右键：查询该 PID 的端口、复制 PID / 路径 / 命令行、打开文件位置、确认后结束进程 / 树 / 应用。
- 端口右键：复制 PID / 端点 / 路径、跳转精确进程身份、确认后结束占用进程。
- Ctrl+E 导出当前页面的筛选结果，端口 CSV 包含 TCP/UDP、IPv4/IPv6、地址、端口、状态、PID 和身份说明。

图标仅为可见行在一个后台工作线程读取，使用有容量和 TTL 上限的缓存；端口页只在打开时每 2 秒后台刷新。UI 不逐行同步读取图标、命令行或文件签名。没有 WebView 或浏览器运行时。

PowerShell 5.1 / 7 一键下载与安装、指定版本、便携版和自动镜像回退见 [安装说明](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/installation.zh-CN.md)。镜像只下载公开资产；版本与 SHA-256 始终取自官方项目发布源。

## 功能

| 页面 | v0.3 已实现能力 |
| --- | --- |
| 进程 / 详细信息 | 虚拟表格、名称/路径/PID/用户搜索、数值排序、进程树与折叠、列显隐、CPU/工作集/GPU/显存/I/O/逐进程网络列，线程/句柄/私有提交/累计 CPU 时间/会话等详细列；文件定位、结束进程或进程树、优先级、当前处理器组亲和性、效率模式、运行新任务和 CSV 导出 |
| 端口与连接 | TCP/UDP IPv4/IPv6 端点、端口/PID/协议/状态筛选、右键复制/跳转/结束、CSV 导出；身份不确定时说明原因 |
| 选中进程详情 | 按需异步读取真实命令行、x86/x64 等架构、WOW64 状态、文件描述/公司/产品/版本资源；可手动重新读取；按需查询当前磁盘文件的离线 Authenticode 嵌入式/目录签名 |
| 性能 | CPU/内存/多 GPU/磁盘/网络实时曲线、120 次采样显示历史、逐逻辑处理器负载；系统线程/句柄/进程数、提交内存及限制、缓存与内核内存等系统可提供的数据 |
| 设备与传感器 | CPU 型号/核心/缓存、原生处理器组/核心/NUMA/效率类别/缓存拓扑、供电与电池信息、主板/BIOS、DIMM 容量/速率/槽位/厂商、物理磁盘型号/总线/固件、网卡硬件/链路/驱动、GPU 驱动/PCI/显示设备；支持设备的 NVIDIA NVML 传感器 |
| DirectX 12 | 按显卡 LUID 查询 D3D12 支持、Feature Level、Shader Model、光追/Mesh Shader/Sampler Feedback 层级、Enhanced Barriers 和节点数，区分软件与虚拟显示设备，显示查询失败原因 |
| 服务 | SCM 服务、状态/PID/启动类型/配置；选中后启动、停止或重启，后台执行并显示权限/状态错误 |
| 启动应用 | HKCU/HKLM Run 的 32/64 位视图及用户/公共启动文件夹，读取 StartupApproved 状态并启用/禁用，保留原启动命令 |
| 用户 | WTS 真实登录会话、用户名/会话状态/进程和资源聚合；带确认的断开与注销操作 |
| 应用历史 | 观察期间累计 CPU 时间与 I/O，保留已观察的结束进程并按应用聚合、本地持久化/清空；ETW 可用时累计实际网络字节及有效覆盖时长 |
| 设置 | 0.5 / 1 / 2 秒刷新、主题、置顶、列显隐本地保存、管理员入口、自动/手动检查更新、一键下载安装 |

结束进程、结束树、服务、启动项及会话管理提供明确的目标与确认。进程操作用 PID 与原始创建 FILETIME 核验，在已验证的句柄上执行；关键系统进程及本应用受保护。进程树根据真实父 PID 和出生时间建立，搜索时显示匹配列表。批量采集、WMI/DirectX 查询、网络、详情读取和管理操作在后台执行，UI 使用不可变采集快照。

## 指标来源与边界

- **CPU / 进程**：Native `NtQuerySystemInformation` 批量读取真实进程 CPU 累计时间、线程、句柄、工作集、私有提交及 I/O 等计数，使用真实采样间隔计算速率；不可用时保留回退/失败说明。CPU 百分比以整台机器为 100%，首轮不足有效样本时显示采样中。
- **内存 / I/O**：默认进程内存为工作集，详细列提供私有提交。共享页可能在多个工作集中重复计算，不能把各进程工作集之和当作物理内存使用量。系统提交/缓存/内核内存来自 Win32 性能信息。进程 I/O 包括文件、设备和其他读取/写入；卷指标由 `IOCTL_DISK_PERFORMANCE` 提供。字节按 1024 换算为 KB/MB/GB，吞吐为字节/秒。
- **GPU**：DXGI/D3DKMT 提供适配器身份和容量；WDDM/PDH 按 LUID、节点和引擎聚合负载，显示最忙引擎。计数与应用使用 DirectX 11、DirectX 12 等图形 API 的关系由 WDDM 决定，实际字段取决于驱动。整卡显存使用 `GPU Adapter Memory`，不把 DXGI 的本应用显存预算当作整卡用量。
- **GPU 计数异常**：Windows 进程 GPU 内存存在[微软记录的累积计数错误](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value)。单进程专用显存超过对应显卡已知正容量时显示不可用并说明原因；整卡数据独立保留。该校验只能识别明显异常。
- **硬件 / 传感器**：静态设备由 WMI/Win32 查询并缓存，可手动刷新；型号、容量和驱动字段保留系统来源。NVML 在可准确匹配的 NVIDIA 设备上读取温度、功耗/功率上限、风扇百分比、核心/显存频率和 PCIe 链路信息。AMD/Intel 提供通用设备信息，厂商传感器接口尚未接入；CPU 温度等也只有系统实际提供时显示。
- **逐进程网络**：后端使用 Kernel-Network ETW 与 TDH schema 解码 TCP/UDP、IPv4/IPv6 的事件字节，按 PID 与精确创建时间关联，拒绝 PID 重用继承，按实际间隔计算速率。通常需要管理员/内核 ETW 权限。无权限、丢事件或 schema 失败时显示 `—` 与状态；成功采到的空闲值为 0。普通权限拒绝路径与本机管理员自有回环已实测：自身 PID 的累计发送/接收各 163,840 字节，私有追踪句柄清理通过；四协议 schema 已单元测试，真实 trace 出现 TCP/UDP IPv4/IPv6 四类事件。四类事件计数属于整个 trace，尚未独立验证自有 PID 的每协议字节与丢失；长期负载、其他 Windows schema、外网与不同环境仍需验证。
- **应用历史**：CPU/I/O 从首次观察的累计计数基线计算，未运行本应用或两次采样之间出生又结束的进程不会凭空补记。网络历史累加完整 ETW 周期的实际字节，不积分已舍入的速率；“网络有效进程秒”是各实例有效覆盖时长之和，有权限或丢事件缺口时明确提示。本地历史有容量上限，不是 Windows 全系统审计日志。
- **进程详情**：只为选中的精确身份加载，不每秒遍历所有进程的命令行和文件资源。公司名称来自 EXE 的版本资源；按需离线 WinVerifyTrust 查询另行展示真实签名者与可信状态，不在线检查吊销；远程进程当前目录没有采用不稳定的 PEB 读取方式，当前显示不可用。
- **管理覆盖范围**：CPU 亲和性操作限定当前处理器组；启动项尚未覆盖所有计划任务、UWP/打包应用及第三方登录触发器，也没有启动影响测量。用户页提供会话聚合与管理，逐用户展开进程等仍在路线图中。

`—` 表示权限、驱动、系统能力或样本条件未满足，不用假数补齐。尚未证明与 Windows 任务管理器完全等同，也没有全面性能对比结果。

端口数据来自 Windows IP Helper 的四类 Owner-PID 表；端点与进程信息不是原子快照，只有验证过 PID 与原始创建 FILETIME 的目标才允许管理。UDP 不显示系统接口未提供的远端地址或 TCP 状态。应用组 GPU 当前显示成员最大值，不能解释为整组引擎合并负载。

## 一键更新

应用运行时默认每 6 小时后台检查项目的稳定版本，失败后重试，可在设置关闭自动检查或立即手动检查。后台检查只检查版本；点击“一键更新”后才执行下载和安装。

下载器核验项目发布地址、稳定版本、安装包名称、大小和 SHA-256。安装助手复制到 `%LOCALAPPDATA%\SystemMonitorX\updates\helper-{标识}`，保留完整运行库，再次核验并锁住 MSI，获取安装互斥锁，确认就绪后主窗口正常退出。助手最多等待旧 PID 的精确 FILETIME 身份退出 30 秒，使用系统 Windows Installer 显示安装进度、禁止自动重启电脑，保存安装日志与结果。

安装成功后启动实际安装版；便携版的一键更新也会安装并启动当前用户安装版。取消或安装失败后重新打开原应用。助手缺失、验证失败或另一个窗口正在安装时，当前窗口保持打开并显示原因。安装结果保存在 `%LOCALAPPDATA%\SystemMonitorX\updates\last-install-result.json`，各次日志位于对应 helper 缓存目录。

v0.2.1 修复 canonical 化后的 `\\?\` 扩展路径进入 Windows Installer 命令行、导致 SourceDir 1314 和安装返回 1619 的问题。**已有 v0.2.0 的用户请下载 v0.2.1 MSI 安装一次**：旧版已复制的旧助手不会因下载新版 MSI 自动修复。

已验证的两条路径：修复后的 v0.2.1 优化 `ui-test` 构建完成公开 v0.2.0 的真实检查、下载、SHA-256 校验、Ready 状态原生安装点击、助手安装（返回 0）及自动启动正式 v0.2.0 安装版，其主程序与助手哈希匹配公开 ZIP；随后，校验过的公开 v0.2.1 MSI 直接从正式 v0.2.0 升级成功（返回 0），正式 v0.2.1 已启动。登记的每用户安装目录、主程序和助手的 0.2.1 版本及公开 ZIP 哈希、开始菜单快捷方式目标均核验通过。

自动发现新版（Newer）→下载→安装完整分支尚未完成端到端验证。取消/失败恢复、离线与多窗口等场景仍需回归。v0.2.0 的 23 项优化原生 UI 检查与响应测量见下文，详细状态见 [ROADMAP.md](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)。

## 快捷键与本地数据

| 快捷键 | 操作 |
| --- | --- |
| Ctrl + F | 搜索 |
| Ctrl + N | 运行新任务 |
| Ctrl + E | 导出 CSV（系统保存对话框） |
| Ctrl + P | 暂停/恢复显示；后台仍采集 |

设置：`%LOCALAPPDATA%\SystemMonitorX\settings.json`。应用历史：`%LOCALAPPDATA%\SystemMonitorX\history-v1.json`。更新缓存和安装日志保存在同目录的 `updates` 子目录。版本检查访问本项目的 GitHub 发布地址，不上传进程或设备遥测。

## 开发与验证

v0.3 源码回归共 154 项通过、2 项环境测试默认忽略，严格 Clippy 通过。PowerShell 5.1/7 各 29 项自测通过。优化原生窗口 33 项检查通过，覆盖真实图标、端口查询、右键菜单与精确身份跳转。994 个真实进程下，230 次逐键 CPU 侧 p50 为 4.19 ms、p95 为 5.85 ms、最大 10.54 ms；30 次显式 render 阶段 p95 为 5.33 ms。结果含测试观测开销，不是 GPU 呈现延迟或官方任务管理器的同条件比较。界面当前仍主要使用中文；国际化 API/Unicode 排序已核查，完整语言切换和可访问性仍在路线图中。


需要 Rust 1.96.0、Visual Studio **使用 C++ 的桌面开发** 工作负载与 Windows 10/11 SDK。打包需要 .NET SDK 和 WiX 5.0.2；依赖由 `Cargo.lock` 锁定。

```powershell
. ./scripts/Enter-DevEnvironment.ps1
cargo run --bin SystemMonitorX
./scripts/Verify.ps1
./scripts/Package.ps1
```

环境脚本寻找完整的 x64 C++ 工具链。验证脚本执行格式、Clippy、测试和真实系统诊断；进程修改验证只操作它自己创建的专用子进程。打包检查 MSI 属性、每用户目标目录、快捷方式及提取后的文件哈希。GitHub Actions 在 Windows 构建，版本标签触发发布；实际结果以构建日志和 Release 资产为准。

```powershell
cargo run --bin monitor_probe -- --smoke --verify-operations --output target/smoke-report.json
cargo run --bin monitor_probe -- --benchmark --output target/benchmark-report.json
```

可选原生 UI 冒烟使用真实 Windows 窗口和 D3D11，验证导航、搜索、选择、弹窗、暂停、详情和窗口尺寸，并输出响应测量。计时为 **CPU 侧阶段耗时**：完整查询批次包含清空、输入、过滤与显式 `render_frame`，每次按键的 key-down + key-up 另行直接计时。分发包含框架同步绘制 dirty view 的开销及 test-support instrumentation；排除 PNG 编码，不是 GPU 实际呈现延迟。独立 10,000 行 filter/sort 压力数据明确标为 synthetic，不注入真实遥测。截图和诊断报告可能包含本机进程及设备信息，输出保存在仓库之外。

2026-10-11，v0.2.0 本机优化 release + `ui-test` 构建的 23 项检查通过，覆盖进程、硬件、DX12、用户、历史、列、进程树、详情和置顶。940 个真实进程下采集 30 组查询批次、230 次实际按键（460 个 key-down/key-up 事件），百分位使用 nearest-rank：

| CPU 阶段 | 样本数 | p50 | p95 | 最大值 |
| --- | ---: | ---: | ---: | ---: |
| 每次按键直接计时 | 230 | 5.3164 ms | 6.6954 ms | 8.0354 ms |
| 显式 `render_frame` | 30 | 4.7491 ms | 7.7874 ms | 7.8613 ms |
| 完整 14 字符 `SystemMonitorX` 查询批次 | 10 | 87.8399 ms | 106.6616 ms | 106.6616 ms |

完整查询批次不能当作单键延迟；这些是单机带测试观测开销的结果，不构成官方任务管理器性能对比或“完全不卡”的证明。

```powershell
cargo run --release --features ui-test --bin SystemMonitorX -- --ui-smoke-dir C:/Temp/SystemMonitorX-ui-check
```

v0.2.1 格式与 Clippy 检查通过，源码测试 91 项通过、1 项默认忽略；上述 v0.2.0 原生 UI 及性能结果、修复版助手实测和公开 v0.2.1 MSI 升级分别保留验证范围。本地检查已通过，云端完整回归以 [Actions 对应运行记录](https://github.com/neko233-com/SystemMonitorX/actions) 为准；取消/失败恢复、完整自动新版分支及长时性能仍需验证。本机管理员 ETW 的自有回环累计字节与清理已单独通过，每协议字节/丢失、长期负载和跨版本回归仍未完成；也没有对官方任务管理器的同条件性能基线。

## 项目结构

```text
src/ui/                    GPUI Kit 界面、虚拟表格、管理及原生 QA
src/monitor.rs             后台系统快照与采样
src/performance.rs         Native 进程计数、持久历史与采集基准
src/network_process.rs     Kernel-Network ETW / TDH
src/hardware.rs            WMI / Win32 静态设备清单
src/gpu.rs                 DXGI / D3DKMT / PDH / NVML
src/directx.rs             按显卡查询 D3D12 能力
src/process_details.rs     单个精确进程身份的按需详情
src/process_tree.rs        稳定进程树投影
src/process_groups.rs      原生窗口身份与应用分组
src/name_order.rs          统一稳定名称排序缓存
src/network_ports.rs       IP Helper TCP/UDP 端口身份
src/cpu_topology.rs        原生处理器/NUMA/缓存拓扑
src/file_signature.rs      离线 Authenticode 查询
src/platform.rs            进程、服务、启动项与会话管理
src/sessions.rs             WTS 会话与资源聚合
src/updater.rs              稳定版检查与校验下载
src/update_install.rs       更新任务、MSI 交接与结果/重启
src/bin/update_helper.rs    独立安装助手
src/bin/monitor_probe.rs    无界面诊断与受控操作验证
packaging/                 WiX 每用户 MSI
scripts/                   环境、验证与打包
```

## 技术来源

- [Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit)：原生控件、虚拟表格、图表与主题。
- [sysinfo](https://docs.rs/sysinfo/0.39.6/sysinfo/)：跨平台采集和 Windows 回退能力。
- [Rust for Windows](https://github.com/microsoft/windows-rs)：Win32/DXGI/Direct3D 类型绑定。
- [Microsoft GPU 性能统计说明](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/)：引擎与显存计数含义。
- [NVIDIA NVML](https://docs.nvidia.com/deploy/nvml-api/index.html)：设备传感器接口。
- [Windows Installer 参数](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/msiexec)：安装进度、重启与日志。
- [WiX Toolset](https://docs.firegiant.com/wix/)：MSI 打包。

Apache-2.0，详见 [LICENSE](LICENSE)。

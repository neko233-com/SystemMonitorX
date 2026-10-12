# SystemMonitorX

[English](README.md) | 简体中文

面向 Windows 10/11 的原生系统监控与进程管理器，使用 **Rust 2024 + Longbridge GPUI Kit 0.7.3**。公开仓库：[neko233-com/SystemMonitorX](https://github.com/neko233-com/SystemMonitorX)。

**v0.4.0 增加八种界面语言**，可自动解析 Windows 界面语言偏好，也可在设置中即时切换并保存。已有功能包括稳定名称排序、真实 EXE 图标、开发者进程/端口工作流、应用分组、CPU 原生拓扑、供电信息、离线签名查询和 PowerShell 安装脚本。验收状态与剩余目标见[路线图](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)。

## 下载与安装

在 [Releases](https://github.com/neko233-com/SystemMonitorX/releases) 选择实际已发布的版本。v0.4.0 包的文件名为：

- **SystemMonitorX-0.4.0-x64.msi**：当前用户安装、升级和卸载，创建开始菜单快捷方式。
- **SystemMonitorX-0.4.0-x64-portable.zip**：完整解压后运行 `SystemMonitorX.exe`。
- **SHA256SUMS.txt**：MSI 与 ZIP 的 SHA-256 校验值。

支持 Windows 10 1803（Build 17134）及以上 / Windows 11 x64，需要支持 GPUI Direct3D 渲染的显卡和驱动。默认目录为 `%LOCALAPPDATA%\Programs\SystemMonitorX`。请保留主程序旁的 `SystemMonitorXUpdate.exe` 和全部随包 VC 运行库 DLL。

Windows PowerShell 5.1 / PowerShell 7 一键安装或升级：

~~~powershell
& ([ScriptBlock]::Create((irm 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1'))) -Mirror Auto
~~~

这条命令选择**实际已发布的最新稳定版**；指定版本也必须存在于 Releases。先保存并审阅脚本、指定版本、仅下载、便携解压和单次本机代理用法见[安装说明](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/installation.zh-CN.md)。官方 GitHub 优先，失败后有限镜像顺序回退；元数据与 SHA-256 始终来自官方项目发布源。脚本不更改全局执行策略、代理、证书信任或防火墙。

应用默认以普通用户权限运行。管理员入口由用户主动选择，Windows 负责权限确认。不会修改默认任务管理器注册或系统快捷键。发布包目前未做代码签名。

<!-- V0.4_SOURCE_NATIVE_FREEZE: 打包前冻结已验证的源码/原生结果；实际发布包/安装验收在在线路线图维护。 -->
**v0.4.0 当前验证：**源码测试 187 项通过（库 131 + GUI 源码回归 56），2 项环境相关测试默认忽略。真实 Windows/D3D11 原生 QA **34/34 项通过**，完成 **8 次设置语言切换与 40 张截图（1040×640 逻辑像素、1.5× 缩放）**。实际 v0.4.0 发布包与安装验收在[在线路线图](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)维护。历史公开 v0.3.0 已通过 PS5.1 安装、全部安装文件/快捷方式核验和国际字符路径便携运行；这些结果不代表 v0.4.0 已安装通过。

## 界面语言与国际使用

| 语言 | 保存的 locale |
| --- | --- |
| 简体中文 | `zh-CN` |
| English | `en-US` |
| 日本語 | `ja-JP` |
| 한국어 | `ko-KR` |
| Deutsch | `de-DE` |
| Français | `fr-FR` |
| Español | `es-ES` |
| Português (Brasil) | `pt-BR` |

默认**跟随系统**会将 Windows 界面语言偏好解析为已支持的语言，无法匹配时回退英文。通过**设置 → 界面语言**即可即时切换，并保存到下次启动。

切换语言会更新应用标签、对话框、缓存反馈和已有硬件展示。进程/设备名称、路径、PID/创建身份、账户名及原始 Windows 诊断保留来源值。当前搜索、精确选择、排序与列宽保持不变。硬件清单使用已有缓存在后台重新格式化，不会因此再次查询 WMI/设备；请求代次检查防止旧语言结果覆盖新展示。

搜索提示和语法针对当前页面，输入只筛选该页模型；导航到另一页会清空查询。**CSV 表头固定英文**，便于脚本和工具稳定处理。名称使用一致的 Unicode 小写排序，不是地区语言排序；当前数字/单位展示仍未完整实现地区格式。应用保存的语言设置只控制应用界面。MSI 自有产品与安装条件文案使用英文；Windows Installer 系统消息、UAC 和原生系统对话框遵从 Windows 自身语言设置。完整键盘与屏幕阅读器可访问性仍在路线图中。

## 面向开发者

列表默认按一致的 Unicode 小写名称顺序排序，同名行以 PID 与出生身份稳定排序。指标刷新复用名称顺序，CPU 变化不会不断移动名称排序的行。需要按 CPU、内存等数值排序时可主动点击对应列标题。

| 任务 | 工作流 |
| --- | --- |
| 查进程 | 普通文本、`pid:1234`、`ppid:1234`、`name:chrome`、`path:projects` |
| 查端口占用 | `port:8080`、`pid:1234`、`tcp listen`、`udp v6`；空格组合多个条件 |
| 进程右键 | 查询端口、复制 PID/路径/命令行、打开文件位置、确认后结束进程/树/应用 |
| 端口右键 | 复制 PID/端点/路径、跳转捕获的精确进程身份、确认后结束占用进程 |
| 导出 | Ctrl+E：进程/详细信息页导出搜索匹配进程，端口页导出筛选端点，其他页导出完整进程快照；表头固定英文 |

进程/详细信息导出包含全部搜索匹配的原始进程，折叠的应用组或树不会遗漏成员。进程 CSV 包含捕获的原始身份和可用的 CPU、内存、GPU、I/O、网络、线程/句柄与账户字段，可选字段缺失时留空。端口 CSV 包含协议、地址族、端点、状态、PID、路径、创建 FILETIME、查询与身份说明。CSV 转义及公式注入防护保留来源数据的字面含义。

真实图标仅为可见行在一个后台工作线程读取，队列、缓存容量与 TTL 均有限制；端口页只在打开时每 2 秒后台刷新。隐藏页延后显示模型工作。没有 WebView 或浏览器运行时。

## 监控与管理

| 页面 | 能力 |
| --- | --- |
| 进程 / 详细信息 | 虚拟表格、PID、CPU、工作集、GPU、显存、I/O 和逐进程网络；稳定排序、应用分组、进程树与列显隐；优先级、当前处理器组亲和性、效率模式与结束操作 |
| 选中进程详情 | 按需异步命令行、架构/WOW64 与 EXE 版本资源；离线嵌入式/目录 Authenticode 签名查询 |
| 性能 | CPU、内存、多 GPU、磁盘与网络；120 次采样显示历史、逐逻辑处理器负载、进程/线程/句柄数、提交/缓存/内核内存 |
| 设备与传感器 | CPU 处理器组/核心/NUMA/缓存/效率类别、供电与电池、主板/BIOS、DIMM、磁盘、网卡、GPU 驱动/PCI 身份 |
| DirectX 12 | 逐显卡 Feature Level、Shader Model、光追、Mesh Shader、Sampler Feedback、Enhanced Barriers 与节点数 |
| 服务 | SCM 状态/配置；确认后后台启动、停止或重启 |
| 启动应用 | HKCU 原生视图 Run、HKLM 32/64 位 Run、用户/公共启动文件夹；StartupApproved 状态与启用/禁用，保留原始命令 |
| 用户 | WTS 会话与资源聚合；确认后断开或注销 |
| 应用历史 | 观察期间 CPU/I/O 和可用 ETW 字节、已结束进程保留、应用聚合与有限容量持久化 |
| 设置 | 八种语言/跟随系统、0.5 / 1 / 2 秒采样、主题、置顶、列显隐、管理员入口、自动/手动更新检查与一键安装 |

进程管理先核验 PID 与原始创建 FILETIME，再在同一原生句柄上执行。关键系统进程与本应用受保护，整组结束操作核验全部捕获成员。服务状态和启动项选择使用原始枚举/身份，不依据翻译文本，所以切换语言不会改变管理目标。耗时采集、硬件、文件与管理操作均在后台执行，界面使用不可变快照。

## 数据来源与限制

- **CPU / 进程：**原生 `NtQuerySystemInformation` 计数与实际采样间隔，保留回退/失败说明。CPU 按整台机器为 100% 统计；不足有效样本时保持不可用。
- **内存 / I/O：**工作集与私有提交分开表示；共享页可能出现在多个进程工作集中。Win32 提供系统提交/缓存/内核内存，`IOCTL_DISK_PERFORMANCE` 提供卷 I/O。进程 I/O 包含文件、设备与其他操作，不能当作网络流量。字节单位按 1024 换算。
- **GPU：**DXGI/D3DKMT 提供身份，WDDM/PDH 提供引擎/显存计数；支持的 NVIDIA 设备增加 NVML 传感器。AMD/Intel 厂商传感器和通用 CPU 温度覆盖仍不完整。当单进程显存超过对应适配器已知正容量时识别[微软记录的 GPU 进程显存异常](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value)，但仍可能存在其他错误。整卡显存使用适配器计数，不使用本应用的 DXGI 显存预算代替。
- **应用分组：**使用真实窗口与父子/出生身份；GPU 当前显示成员最大值，不是整组各引擎合并负载。应用分类可能与 Windows 有差异。
- **端口：**IP Helper 的 TCP/UDP IPv4/IPv6 Owner-PID 表。端点与进程读取不是原子快照；身份不确定的行保留原因并禁止从该行结束目标。UDP 不伪造远端端点或 TCP 状态，端点列表不是流量计数。
- **逐进程网络：**Kernel-Network ETW/TDH 事件字节与精确出生身份关联，通常需要管理员/内核追踪权限。权限、丢事件或 schema 失败时显示不可用。自有回环累计字节与追踪清理已有历史实测；逐协议自有字节、长期外网和更多 schema 仍需回归。
- **历史：**从观察基线开始，无法重建两次采样之间出生又结束的进程，也不能补记本应用未运行期间。网络实际字节只累计有效 ETW 周期，保留覆盖缺口，存储有容量上限。
- **硬件 / 签名：**缓存 WMI/Win32 数据，可手动刷新。离线 WinVerifyTrust 使用本地嵌入式/目录证据，不在线检查吊销。版本资源 CompanyName 与真实可信签名者分开；缺失签署时间保持未知。
- **管理：**亲和性限定当前处理器组。全部计划任务、打包应用和其他登录启动来源、启动影响测量、逐用户进程展开尚未完成。
- **DirectX：**界面仍由 **D3D11** 渲染，D3D12 能力查询不会切换渲染后端。未实现稳定公开的远程进程当前目录读取。

缺失的可选测量保持不可用。尚未证明支持所有硬件、与任务管理器完全等同，或在所有场景具有性能优势。

## 更新

应用运行时默认每 6 小时后台检查项目稳定版本，失败后重试，可手动检查或关闭自动检查。自动检查不会自动安装。

下载器核验仓库地址、稳定版本、文件名、大小和 SHA-256。独立助手再次验证并锁定 MSI，获取安装互斥锁，等待旧进程的精确身份退出，再调用 Windows Installer，禁止自动重启。成功后启动正式安装版；取消/失败时尝试重新打开原应用。便携版的一键更新会安装当前用户版本。

日志与 `last-install-result.json` 位于 `%LOCALAPPDATA%\SystemMonitorX\updates`。助手缺失或包不完整时，当前窗口保持打开并显示错误。

v0.2.1 修复旧版扩展路径传入 MSI 的交接失败。v0.2.0 用户应手动安装一次当前 MSI，取得修复助手。修复助手的真实 Ready/下载/校验/安装/重启路径与公开 v0.2.0 → v0.2.1 升级已有验证；自动发现较新版本的完整分支，以及取消/失败恢复，仍需更广回归。

## 快捷键与本地数据

| 快捷键 | 操作 |
| --- | --- |
| Ctrl+F | 搜索当前支持搜索的页面 |
| Ctrl+N | 运行新任务 |
| Ctrl+E | 按当前页面范围导出 |
| Ctrl+P | 暂停/恢复显示，后台仍采集 |

设置与历史保存于 `%LOCALAPPDATA%\SystemMonitorX`。更新检查不上传设备或进程遥测。Windows 路径使用 UTF-16 API，计数接口不依赖界面语言，名称保留原始 Unicode。英文与完整中文文档相互链接。

## 构建与验证

需要 Rust 1.96.0、Visual Studio「使用 C++ 的桌面开发」、Windows SDK、翻译验证门禁所需 Python 3（仅用标准库），以及打包所需 .NET SDK / WiX 5.0.2。Python 仅用于开发验证，不随应用分发。`Cargo.lock` 锁定依赖。

~~~powershell
. ./scripts/Enter-DevEnvironment.ps1
cargo run --bin SystemMonitorX
./scripts/Verify.ps1
./scripts/Package.ps1
~~~

验证覆盖格式、严格 Clippy、测试、安装脚本自测与真实遥测。修改操作测试仅针对专用自有子进程。打包核验 MSI 属性、当前用户目录、快捷方式和提取文件哈希。版本标签触发 Windows 构建发布，结果见 [Actions](https://github.com/neko233-com/SystemMonitorX/actions)。

可选原生 Windows GPUI/D3D11 界面回归：

~~~powershell
cargo run --release --features ui-test --bin SystemMonitorX -- --ui-smoke-dir C:/Temp/SystemMonitorX-ui-check
~~~

当前 v0.4.0 源码测试 **187 项通过，2 项默认忽略**。PowerShell 安装脚本有 29 项离线自测，历史上已分别在 PS5.1 / PS7 运行。真实 Windows/D3D11 原生 QA **34/34 项通过**，完成 **8 次设置语言切换与 40 张截图（1040×640 逻辑像素、1.5× 缩放）**。人工复核覆盖八语言的进程页和设置页（**16/40 张截图**），其余 24 张由自动原生检查覆盖。该版本实际发布包/安装验收在[在线路线图](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)维护。报告与截图可能包含本机进程/设备信息，应保留在私有位置。

最终 v0.4.0 原生测试观察到 **806 个真实进程**。**230 次逐键**键盘分发/过滤 CPU 耗时为 p50 **3.6232 ms**、p95 **5.6238 ms**、max **9.5156 ms**；**30 次 render** 的 `render_frame` CPU 耗时为 p95 **6.2899 ms**、max **6.7198 ms**。测量包含测试观测开销，排除 PNG 编码和 GPU 实际呈现，不是官方任务管理器同条件比较。独立的 10,000 行 synthetic 模型不会注入真实遥测。完整原生测试范围与历史结果见 [ROADMAP.zh-CN.md](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)。

## 技术来源

[Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit)、[sysinfo](https://docs.rs/sysinfo/0.39.6/sysinfo/)、[Rust for Windows](https://github.com/microsoft/windows-rs)、[Microsoft GPU 统计](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/)、[NVIDIA NVML](https://docs.nvidia.com/deploy/nvml-api/index.html)、[Windows Installer](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/msiexec)、[WiX](https://docs.firegiant.com/wix/)。

Apache-2.0，详见 [LICENSE](LICENSE)。

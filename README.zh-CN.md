# SystemMonitorX

[English](README.md) | 简体中文

**看清电脑资源占用，找到进程和端口对应的程序，直接进行管理。**

[下载 Windows 安装包](https://github.com/neko233-com/SystemMonitorX/releases/download/v0.5.0/SystemMonitorX-0.5.0-x64.msi) · [免安装版](https://github.com/neko233-com/SystemMonitorX/releases/download/v0.5.0/SystemMonitorX-0.5.0-x64-portable.zip)

| 你遇到的情况 | 直接这样用 |
| --- | --- |
| 电脑变慢，想看看谁占资源 | 打开**概览**，点击主要 CPU / 内存进程继续查看 |
| 想找到某个程序或 PID | 打开**进程**，输入名称或 `pid:1234` |
| 开发服务提示端口被占用 | 打开**端口与连接**，输入 `port:3000`，右键查看对应进程 |
| 不知道一个进程是什么 | 右键**打开文件位置**，或打开**进程工具**查看模块 |

![概览：资源占用和主要进程](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/overview-zh.png)

*真实 Windows 原生窗口渲染的匿名示例数据，用于说明界面；不是本机实时遥测或性能测试。*

面向 Windows 10/11 的原生系统监控与进程管理器，使用 **Rust 2024 + Longbridge GPUI Kit 0.7.3**。公开仓库：[neko233-com/SystemMonitorX](https://github.com/neko233-com/SystemMonitorX)。

**v0.5.0 增加简明概览、五列进程视图、保存查询、快捷操作和按需开发者诊断。** 先看资源占用与主要使用者，需要排查时再打开详细工具。继续支持八种界面语言、稳定名称排序、真实进程图标/PID、CPU/GPU/硬件信息及经过校验的更新下载。验收状态与剩余目标见[路线图](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)。

## 下载与安装

只想马上使用：**下载 MSI → 双击安装 → 在开始菜单打开 SystemMonitorX**。打开后先看概览；检查更新和一键更新位于**设置**。不想安装就下载 ZIP，完整解压后运行里面的 `SystemMonitorX.exe`。

在 [Releases](https://github.com/neko233-com/SystemMonitorX/releases) 选择实际已发布的版本。v0.5.0 包的文件名为：

- **SystemMonitorX-0.5.0-x64.msi**：当前用户安装、升级和卸载，创建开始菜单快捷方式。
- **SystemMonitorX-0.5.0-x64-portable.zip**：完整解压后运行 `SystemMonitorX.exe`。
- **SHA256SUMS.txt**：MSI 与 ZIP 的 SHA-256 校验值。

支持 Windows 10 1803（Build 17134）及以上 / Windows 11 x64，需要支持 GPUI Direct3D 渲染的显卡和驱动。默认目录为 `%LOCALAPPDATA%\Programs\SystemMonitorX`。请保留主程序旁的 `SystemMonitorXUpdate.exe` 和全部随包 VC 运行库 DLL。

Windows PowerShell 5.1 / PowerShell 7 一键安装或升级：

~~~powershell
& ([ScriptBlock]::Create((irm 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1'))) -Mirror Auto
~~~

这条命令选择**实际已发布的最新稳定版**；指定版本也必须存在于 Releases。先保存并审阅脚本、指定版本、仅下载、便携解压和单次本机代理用法见[安装说明](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/installation.zh-CN.md)。官方 GitHub 优先，失败后有限镜像顺序回退；元数据与 SHA-256 始终来自官方项目发布源。脚本不更改全局执行策略、代理、证书信任或防火墙。

应用默认以普通用户权限运行。管理员入口由用户主动选择，Windows 负责权限确认。不会修改默认任务管理器注册或系统快捷键。发布包目前未做代码签名。


## 从这里开始

1. 打开**概览**，查看 CPU、内存、GPU、磁盘占用及本次观察到的主要使用者。这里提供采样摘要，不将其当作电脑健康诊断。
2. 打开**进程**，按名称找到应用。全新设置默认显示**名称、PID、CPU、内存、GPU**五列；通过**选择进程列 → 常用列 / 全部列**切换详细程度。已有列显示偏好会保留。
3. 按 **F1** 打开应用内帮助，按 **Ctrl+Shift+P** 查找页面或操作，按 **F5** 刷新当前页面。未取得的测量值显示 **—**，不会猜测数值。

## 界面示例

**进程：**默认只显示名称、PID、CPU、内存和 GPU。名称按 A–Z 保持稳定，读数更新时不会因为占用变化不断跳动。

![进程：图标、PID 和五个常用列](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/processes-zh.png)

<details>
<summary>需要排查端口或深入了解进程时，展开这两个例子</summary>

**端口：**输入 `port:3000` 查占用，或 `pid:1234` 查该进程的连接；右键可查看进程或结束占用者，结束操作需要确认。

![端口：TCP/UDP 和对应进程](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/ports-zh.png)

**进程工具：**右键进程打开工具，查看模块和虚拟内存元数据，复制查询命令或保存诊断 JSON。复制查询不会自动执行。

![进程工具：按需诊断和导出](https://raw.githubusercontent.com/neko233-com/SystemMonitorX/v0.5.0/assets/screenshots/process-tools-zh.png)

以上均为匿名示例数据。更详细的操作见[开发者指南](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/developer-tools.md)。

</details>

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
| 保存查询 | 进程与端口查询合计最多八条，可保存、重新使用和删除；在对应页面显示 |
| 进程工具 | 只读 DLL/模块与虚拟地址诊断、核验身份的 PowerShell 查询、主动打开终端、复制/保存诊断 JSON |
| 导出 | Ctrl+E：进程/详细信息页导出搜索匹配进程，端口页导出筛选端点，其他页导出完整进程快照；表头固定英文 |

进程/详细信息导出包含全部搜索匹配的原始进程，折叠的应用组或树不会遗漏成员。进程 CSV 包含捕获的原始身份和可用的 CPU、内存、GPU、I/O、网络、线程/句柄与账户字段，可选字段缺失时留空。端口 CSV 包含协议、地址族、端点、状态、PID、路径、创建 FILETIME、查询与身份说明。CSV 转义及公式注入防护保留来源数据的字面含义。

诊断 JSON 只捕获选中的进程及 PID/创建 FILETIME 都匹配的端点，使用固定英文键、schema 版本、采样时间/来源与 null 缺失值。默认不含命令行和环境变量；路径、账户名及网络地址仍可能涉及本机信息。Windows 创建 ticks 使用十进制字符串，避免 JSON 使用方丢失精确身份。复制 PowerShell 查询不会执行命令；只有主动点击**在程序目录打开 PowerShell**才会启动终端，目录是可执行文件所在文件夹，不假装读取远程进程当前工作目录。诊断及启动参数见双语[开发者指南](https://github.com/neko233-com/SystemMonitorX/blob/main/docs/developer-tools.md)。

真实图标仅为可见行在一个后台工作线程读取，队列、缓存容量与 TTL 均有限制；端口页只在打开时每 2 秒后台刷新。隐藏页延后显示模型工作。没有 WebView 或浏览器运行时。

## 监控与管理

| 页面 | 能力 |
| --- | --- |
| 概览 | 本次 CPU/内存/GPU/磁盘采样摘要、观察到的主要资源使用者、容量提示与详细页面入口 |
| 进程 / 详细信息 | 虚拟表格、PID、CPU、工作集、GPU、显存、I/O 和逐进程网络；稳定排序、应用分组、进程树与列显隐；优先级、当前处理器组亲和性、效率模式与结束操作 |
| 选中进程详情 | 按需异步命令行、架构/WOW64 与 EXE 版本资源；离线嵌入式/目录 Authenticode 签名查询；只读 DLL 与虚拟地址诊断及 JSON 导出 |
| 性能 | CPU、内存、多 GPU、磁盘与网络；120 次采样显示历史、逐逻辑处理器负载、进程/线程/句柄数、提交/缓存/内核内存 |
| 设备与传感器 | CPU 处理器组/核心/NUMA/缓存/效率类别、供电与电池、主板/BIOS、DIMM、磁盘、网卡、GPU 驱动/PCI 身份 |
| DirectX 12 | 逐显卡 Feature Level、Shader Model、光追、Mesh Shader、Sampler Feedback、Enhanced Barriers 与节点数 |
| 服务 | SCM 状态/配置；确认后后台启动、停止或重启 |
| 启动应用 | HKCU 原生视图 Run、HKLM 32/64 位 Run、用户/公共启动文件夹；StartupApproved 状态与启用/禁用，保留原始命令 |
| 用户 | WTS 会话与资源聚合；确认后断开或注销 |
| 应用历史 | 观察期间 CPU/I/O 和可用 ETW 字节、已结束进程保留、应用聚合与有限容量持久化 |
| 设置 | 八种语言/跟随系统、0.5 / 1 / 2 / 5 秒采样、主题、置顶、列显隐、管理员入口、自动/手动更新检查与一键安装 |

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
- **进程诊断：**按需在后台读取 Toolhelp 模块快照和 VirtualQueryEx 区域元数据，并核验精确身份。默认上限为 4,096 个模块、16,384 个区域及两秒协作预算，预算在 Windows 调用之间检查，单次原生调用仍可能超过预算。不读取目标内存内容，不暂停或修改进程。虚拟地址大小、保留/提交元数据不等于实际驻留 RAM，也不能直接判断内存泄漏。部分/不可用结果保留原因，权限、位数和模块列表变化可能限制覆盖。
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
| Ctrl+Shift+P | 查找页面或操作 |
| F1 | 打开应用内帮助 |
| F5 | 刷新当前页面；遥测页面使用最新已采集快照 |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | 概览 / 进程 / 端口 |
| Ctrl+F | 搜索当前支持搜索的页面 |
| Ctrl+N | 运行新任务 |
| Ctrl+E | 按当前页面范围导出 |
| Ctrl+P | 暂停/恢复显示，后台仍采集 |

设置与历史保存于 `%LOCALAPPDATA%\SystemMonitorX`。更新检查不上传设备或进程遥测。Windows 路径使用 UTF-16 API，计数接口不依赖界面语言，名称保留原始 Unicode。英文与完整中文文档相互链接。

## 构建与验证

**v0.5.0 源码/原生验收：**238 项源码测试通过（库 178 + GUI 60），2 项环境相关测试仍默认忽略。54 项真实 Windows/D3D11 交互检查全部通过，其中包含 1040×640 逻辑像素、1.5× 缩放下的专门最小窗口和八语言验收；本进程模块/内存 10 项检查、安装脚本 29 项自测通过。公开安装包及本机安装验收在[在线路线图](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)维护。

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

**历史 v0.4.0 验证：**源码测试 **187 项通过，2 项默认忽略**。PowerShell 安装脚本有 29 项离线自测，历史上已分别在 PS5.1 / PS7 运行。真实 Windows/D3D11 原生 QA **34/34 项通过**，完成 **8 次设置语言切换与 40 张截图（1040×640 逻辑像素、1.5× 缩放）**。人工复核覆盖八语言的进程页和设置页（**16/40 张截图**），其余 24 张由自动原生检查覆盖。历史版本实际发布包/安装验收在[在线路线图](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)维护。报告与截图可能包含本机进程/设备信息，应保留在私有位置。

最终 v0.4.0 原生测试观察到 **806 个真实进程**。**230 次逐键**键盘分发/过滤 CPU 耗时为 p50 **3.6232 ms**、p95 **5.6238 ms**、max **9.5156 ms**；**30 次 render** 的 `render_frame` CPU 耗时为 p95 **6.2899 ms**、max **6.7198 ms**。测量包含测试观测开销，排除 PNG 编码和 GPU 实际呈现，不是官方任务管理器同条件比较。独立的 10,000 行 synthetic 模型不会注入真实遥测。完整原生测试范围与历史结果见 [ROADMAP.zh-CN.md](https://github.com/neko233-com/SystemMonitorX/blob/main/ROADMAP.zh-CN.md)。

## 技术来源

[Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit)、[sysinfo](https://docs.rs/sysinfo/0.39.6/sysinfo/)、[Rust for Windows](https://github.com/microsoft/windows-rs)、[Microsoft GPU 统计](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/)、[NVIDIA NVML](https://docs.nvidia.com/deploy/nvml-api/index.html)、[Windows Installer](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/msiexec)、[WiX](https://docs.firegiant.com/wix/)。

Apache-2.0，详见 [LICENSE](LICENSE)。

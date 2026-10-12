# Developer tools

English | [简体中文](#开发者工具)

[Project](../README.md) · [Installation](installation.md) · [Acceptance and remaining work](../ROADMAP.md)

Start with Overview and Processes. Open Details or Process tools when investigating one process. This guide describes the v0.5.0 workflow; validation results belong to that version's acceptance record.

## Queries, columns and shortcuts

| Page | Example | Meaning |
| --- | --- | --- |
| Processes / Details | `pid:1234` | Match numeric PID exactly |
| Processes / Details | `ppid:1234` | Match parent PID |
| Processes / Details | `name:node` | Process-name prefix |
| Processes / Details | `path:project` | Text in executable path |
| Ports | `port:3000` | Local or remote port |
| Ports | `pid:1234 tcp listen` | Combine process, protocol and state conditions |
| Ports | `udp ipv6` | IPv6 UDP bound endpoints |

Process queries use one prefix whose value is literal; multiple process prefixes are not combined. Ports supports space-separated conditions. Plain text searches the current page. Navigation clears its query; changing interface language preserves it.

Click **Save query** to retain a process or port query. Up to **eight in total** are stored locally and displayed on the corresponding page, using the same A–Z name rule. Click a saved query to reuse it or its remove button to delete it. These are filters, not scripts.

**Columns → Common columns** shows Name, PID, CPU, Memory and GPU. **All columns** restores the columns available on the current page. Details also offers threads, handles, account, private commit, CPU time and session fields. Column visibility does not change collected/exported data. Name sorting stays stable while metrics refresh; click another column to request numeric sorting.

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+P | Find pages and actions |
| F1 / F5 | In-app help / refresh current page |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Overview / Processes / Ports |
| Ctrl+F / Ctrl+N | Search / run a new task |
| Ctrl+E / Ctrl+P | Page-scoped CSV export / pause or resume display |

Pause affects display; collection continues. F5 requests a fresh sample and refreshes the current page’s on-demand data. Paused display stays frozen until resumed. Settings offers **0.5, 1, 2 or 5 second** telemetry sampling. Ports has a separate two-second refresh while open. A missing optional measurement appears as **—**, not a guessed zero.

## Inspect one process

Select the target and open **Process tools** from the inspector or right-click menu. Background inspection checks its PID and exact Windows creation FILETIME, then reports complete, partial or unavailable data.

- **DLLs and loaded modules:** Toolhelp provides original names/paths, base addresses and image sizes. A changing list, cross-bitness limits or access denial can prevent complete enumeration.
- **Virtual address space:** VirtualQueryEx provides region metadata and bounded summaries of committed/reserved/private/executable ranges. Address size and commit are not resident RAM, and do not establish a memory leak or computer-health diagnosis.
- **Limits:** at most **4,096 modules**, **16,384 regions** and a default **two-second cooperative budget** checked between Windows calls. One native call cannot be cancelled by this budget and may exceed it. Truncation/errors retain their reason.

Inspection does not read target memory contents, suspend or modify the process, or duplicate its handles. Sequential module/region observations are not an atomic debugger snapshot. Use normal working-set/private-commit fields to inspect the corresponding process memory counters. Hardware and sensor coverage depends on Windows APIs, permissions and drivers; support for every device is not claimed.

## Diagnostic JSON

**Copy diagnostic JSON** writes to the clipboard; **Save diagnostic JSON** opens a save dialog. The selected immutable snapshot is captured before a dialog returns, so later selection changes cannot silently change the report owner.

The report contains a schema identifier/version, fixed English keys, sampling times, sources, selected process metrics, matched cached details and any captured module/region inspection. Unknown optional values are `null`; measured zero remains zero. Exact creation FILETIME uses a decimal string to avoid JSON consumer integer precision loss.

Endpoints are included only when both PID and creation FILETIME match. Unknown ownership and reused-PID instances are excluded and counted. Uncollected endpoints remain unknown, and an empty matched list does not guarantee a running process has no sockets. IP Helper endpoint tables are independent observations, not traffic counters. UDP has bound endpoints without fabricated remote peers or TCP state.

The normal UI report **excludes command lines and environment variables** and does not dump unrelated processes or host information. Original paths, module/account names and network addresses can still identify local data; review a report before sharing. CSV remains a separate page-scoped export with fixed English headers and formula-injection protection.

## PowerShell

**Copy PowerShell query** copies a read-only selected-process query. **Copy port query** copies TCP and UDP queries for the exact captured process instance. Copying does not run either command. When executed, the generated query holds a process handle, checks exact creation time, reports stale selection/access failure and releases the handle. Port results are current independent tables, not simultaneous process/endpoint sampling.

**Open PowerShell in program folder** launches Windows PowerShell only after that explicit click, without a user profile, and changes directory using a literal path. The executable folder can differ from the process's working directory; no unsupported remote current-directory API is claimed. The terminal does not automatically run a query or termination command.

A PID can be reused. Termination uses a separate confirmed management action and checks original creation time again; unsaved work can be lost. Protected or unavailable operations retain an error rather than silently enabling privileges.

## GUI launch arguments

From the installed or extracted application folder:

~~~powershell
.\SystemMonitorX.exe --pid 1234
.\SystemMonitorX.exe --port 3000 --developer
.\SystemMonitorX.exe --page overview --simple
.\SystemMonitorX.exe --help
~~~

| Argument | Result |
| --- | --- |
| `--pid NUM` | Processes with `pid:NUM`; ASCII decimal 1..4,294,967,295 |
| `--port NUM` | Ports with `port:NUM`; ASCII decimal 1..65,535 |
| `--page NAME` | overview, processes, details, performance, ports, services, startup, users, history, devices, settings |
| `--simple` / `--developer` | Five common columns / all columns available on the page |
| `--help` | In-app help; may accompany one route and one view |

Choose only one route (**--pid**, **--port**, **--page**) and one view (**--simple**, **--developer**). Repeated flags, unknown names/options, missing values, signed/whitespace-padded/non-ASCII numbers and overflow are errors. Flags/page names are case-sensitive; the parser does not guess. No arguments keeps application defaults and saved column choices. These flags route the GUI, without terminating processes or executing shell commands.

---

# 开发者工具

[English](#developer-tools) | 简体中文

[项目](../README.zh-CN.md) · [安装说明](installation.zh-CN.md) · [验收与剩余工作](../ROADMAP.zh-CN.md)

先看概览和进程，需要排查单个目标时再打开详细信息或进程工具。本文说明 v0.5.0 工作流，验证结果以该版本验收记录为准。

## 查询、列显示与快捷键

| 页面 | 示例 | 含义 |
| --- | --- | --- |
| 进程 / 详细信息 | `pid:1234` | 精确匹配数值 PID |
| 进程 / 详细信息 | `ppid:1234` | 匹配父进程 PID |
| 进程 / 详细信息 | `name:node` | 名称前缀 |
| 进程 / 详细信息 | `path:project` | 程序路径包含文字 |
| 端口 | `port:3000` | 本地或远端端口 |
| 端口 | `pid:1234 tcp listen` | 组合进程、协议和状态条件 |
| 端口 | `udp ipv6` | IPv6 UDP 绑定端点 |

进程查询把单个前缀后的内容作为字面值，不组合多个进程前缀；端口支持空格分隔的多条件。普通文本搜索当前页。切换页面清空查询，切换界面语言保留查询。

点击**保存查询**可保留进程/端口筛选，两类合计最多**八条**，保存在本机并在对应页按统一 A–Z 名称规则显示。点击已保存查询重新使用，点击删除按钮移除。这些是筛选条件，不是脚本。

**选择进程列 → 常用列**显示名称、PID、CPU、内存、GPU 五列，**全部列**恢复当前页可用列。详细信息还提供线程、句柄、账户、私有提交、CPU 时间和会话字段。显隐不改变采集/导出数据。名称排序不会因指标更新不断移动行；需要数值排序时点击对应列。

| 快捷键 | 操作 |
| --- | --- |
| Ctrl+Shift+P | 查找页面与操作 |
| F1 / F5 | 应用内帮助 / 刷新当前页 |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | 概览 / 进程 / 端口 |
| Ctrl+F / Ctrl+N | 搜索 / 运行新任务 |
| Ctrl+E / Ctrl+P | 按页面范围导出 CSV / 暂停或恢复显示 |

暂停只影响显示，后台继续采集。F5 请求新采样，并刷新当前页的按需数据；暂停时显示仍冻结，直到恢复。设置提供 **0.5、1、2、5 秒**遥测采样；端口页打开时单独每两秒刷新。缺失可选测量显示 **—**，不会猜成零。

## 检查单个进程

选中目标并打开**进程工具**。后台核验 PID 与精确 Windows 创建 FILETIME 后读取，标明完成、部分数据或不可用。

- **DLL 与已加载模块：**Toolhelp 提供原始名称/路径、基址和映像大小。列表变化、位数限制或访问拒绝可能导致枚举不完整。
- **虚拟地址空间：**VirtualQueryEx 提供区域元数据与已提交/保留/私有/可执行范围的有限摘要。虚拟地址大小、提交量不等于驻留 RAM，不能据此证明内存泄漏或判断电脑健康。
- **限制：**最多 **4,096 个模块**、**16,384 个区域**，默认**两秒协作预算**在 Windows 调用之间检查。单次原生调用无法由该预算取消，可能超过预算。截断和错误保留原因。

不读取目标内存内容，不暂停/修改进程，不复制目标句柄。顺序模块/区域观察不是调试器原子快照；工作集/私有提交查看通常的进程内存计数字段。硬件和传感器覆盖依赖 Windows 接口、权限、驱动，不声称支持全部设备。

## 诊断 JSON

**复制诊断 JSON**写入剪贴板，**保存诊断 JSON**打开保存对话框。选中目标的不可变快照先被捕获，不会因对话框期间改变选择而悄悄改变报告所有者。

报告包含 schema 名称/版本、固定英文键、采样时间、来源、目标指标、身份匹配的缓存详情和已捕获模块/区域诊断。未知可选值为 `null`，真实测得零保留为零。精确创建 FILETIME 使用十进制字符串，避免 JSON 使用方丢失整数精度。

端点只有 PID 与创建 FILETIME 都匹配才纳入；所有者不明或 PID 重用的实例排除并计数。未采集端点保持未知，空匹配列表不保证运行中的进程没有套接字。IP Helper 表是独立观察，不是流量计数；UDP 只提供绑定端点，不伪造远端或 TCP 状态。

正常界面报告**不含命令行和环境变量**，不转储其他进程或主机信息。原始路径、模块/账户名和网络地址仍可能涉及本机身份信息，分享前应检查。CSV 为独立的按页范围导出，保持固定英文表头与公式注入防护。

## PowerShell

**复制 PowerShell 查询**提供所选进程的只读查询，**复制端口查询**提供捕获实例的 TCP/UDP 查询。复制不会执行；运行时持有进程句柄、核验创建时间，过期或访问失败时报告错误，最后释放句柄。端口结果是当时的独立表，不是同时完成的进程/端点采样。

只有主动点击**在程序目录打开 PowerShell**才会启动 Windows PowerShell，不加载用户配置，使用字面路径切换目录。可执行文件目录可能不是进程工作目录，未声称支持远程当前目录接口。终端不会自动执行查询或结束命令。

PID 可能被重用；结束进程使用独立的确认管理操作并再次核验原始创建时间，可能丢失未保存内容。受保护/不可用操作保留错误，不悄悄开启额外权限。

## 图形界面启动参数

在安装或解压后的程序目录：

~~~powershell
.\SystemMonitorX.exe --pid 1234
.\SystemMonitorX.exe --port 3000 --developer
.\SystemMonitorX.exe --page overview --simple
.\SystemMonitorX.exe --help
~~~

| 参数 | 结果 |
| --- | --- |
| `--pid NUM` | 进程页查询 `pid:NUM`；ASCII 十进制 1..4,294,967,295 |
| `--port NUM` | 端口页查询 `port:NUM`；ASCII 十进制 1..65,535 |
| `--page NAME` | overview、processes、details、performance、ports、services、startup、users、history、devices、settings |
| `--simple` / `--developer` | 常用五列 / 当前页可用全部列 |
| `--help` | 应用内帮助，可同时指定一个路由和一种视图 |

**--pid、--port、--page**只能选一种路由，**--simple、--developer**只能选一种视图。重复参数、未知参数/页面、缺少值、带符号/首尾空格/非 ASCII 数字及溢出均明确报错。参数和页面名区分大小写，不猜测。无参数保持通常默认值与已有列显示偏好。参数只导航图形界面，不结束进程或执行 shell 命令。

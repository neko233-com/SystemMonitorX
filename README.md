# SystemMonitorX

Windows 专用的原生任务管理器替代应用，使用 **Rust 2024 + Longbridge GPUI Kit 0.7.1**。

无需 Electron、浏览器或 WebView。GPUI 使用 Windows 原生窗口与 GPU 加速渲染；sysinfo、DXGI、WDDM/PDH 与 Win32 API 负责实际采集和管理。

## 下载与运行

在 [Releases](https://github.com/neko233-com/SystemMonitorX/releases) 下载：

- `SystemMonitorX-0.1.0-x64.msi`：当前用户安装，无需管理员权限，创建开始菜单快捷方式，支持升级和卸载。
- `SystemMonitorX-0.1.0-x64-portable.zip`：解压后运行 `SystemMonitorX.exe`。
- `SHA256SUMS.txt`：安装包和便携包的校验值。

支持 Windows 10 1803（Build 17134）及以上 / Windows 11 x64，需要支持 GPUI 的 Direct3D 显卡和驱动。默认以普通用户运行；需要管理其他用户的进程时，可右键选择“以管理员身份运行”。应用不会自动提升权限，也不会修改默认任务管理器或系统快捷键。

## 已实现功能

| 页面 | 功能 |
| --- | --- |
| 进程 | 全量虚拟表格、名称/路径/PID 搜索、各列排序、CPU、内存、GPU、GPU 显存、I/O 速率、父 PID、进程详情、文件定位、结束任务、优先级、运行新任务、CSV 导出 |
| 性能 | CPU/内存/GPU/磁盘/网络实时曲线、120 次采样历史、逐逻辑处理器使用率、处理器型号、核心数和频率 |
| 设备与传感器 | 系统信息、多 GPU 及整卡专用/共享显存、多卷容量与读写、多网卡吞吐、系统可提供的温度 |
| 服务 | SCM 真实服务列表、名称、显示名称、状态、PID、启动类型、搜索与刷新（只读） |
| 启动应用 | HKCU/HKLM Run（32/64 位视图）及用户/公共启动文件夹、搜索与刷新（只读） |
| 设置 | 0.5 / 1 / 2 秒刷新、深色/浅色主题、本地持久化、快捷键 |

结束任务前有确认；关键系统进程和应用自身受保护。PID 与创建时间共同校验，操作在同一已验证的 Win32 句柄上执行，避免误操作被 Windows 重用的 PID。采集在专用线程执行，UI 只读取快照。

## 指标说明与当前边界

- CPU 与进程 CPU 都以整台机器为 100%；首轮不足两次有效采样时显示“采样中”。
- GPU 使用率按适配器 LUID、物理节点和引擎聚合，显示最忙引擎，不把独立引擎的使用率相加。支持符合 WDDM 的 NVIDIA、AMD 与 Intel；实际可用项取决于驱动和系统计数器。
- 整卡显存来自 `GPU Adapter Memory` 的 PDH 计数器；DXGI 提供显卡名称和容量。DXGI 的“当前进程预算”不被冒充为整卡使用量。间接/虚拟显示设备被过滤。
- 内存采用进程工作集，GPU 进程内存由 WDDM 计数器提供。共享内存/共享资源的统计不能简单相加来计算全系统使用量。
- Windows 的进程 GPU 内存计数器存在[微软记录的累积错误](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value)。当单进程专用显存计数超过该显卡已知的正容量时，应用将该进程 GPU 内存显示为不可用，并在进程/设备页提示；不会裁剪成伪造数值，整卡统计独立保留。此校验只识别明显异常，不能保证发现所有系统计数器错误。
- 字节量按 1024 换算为 KB / MB / GB；吞吐为字节/秒。进程 I/O 是 Windows `GetProcessIoCounters` 的读写传输量，也可能包含文件之外的设备 I/O；磁盘卷指标由 `IOCTL_DISK_PERFORMANCE` 采集。
- `—` 表示权限不足、计数器未提供或样本不足，成功采集到的空闲值显示 0。受保护进程的部分 sysinfo 字段可能由操作系统返回零。
- CPU/GPU 温度、风扇转速、功耗并非所有 Windows 驱动都有通用接口。当前只展示系统可读取的温度，不捏造数值；厂商 NVML/ADL、硬件传感器驱动尚未接入。
- 逐进程网络流量需要 ETW/WFP 采集器，当前支持每网卡和全机网络吞吐。启动项与服务当前为只读；启动项不覆盖所有计划任务、UWP 或第三方登录触发器。
- 这是首个可用版本，不声称已经覆盖 Windows 任务管理器的每个管理功能。安装包目前未做商业证书签名。

## 快捷键

| 快捷键 | 操作 |
| --- | --- |
| Ctrl + F | 搜索 |
| Ctrl + N | 运行新任务 |
| Ctrl + E | 导出 CSV（系统保存对话框） |
| Ctrl + P | 暂停/恢复显示；后台仍采集，恢复后显示最新快照 |

设置保存在 `%LOCALAPPDATA%\SystemMonitorX\settings.json`。不上传遥测或进程信息。

## 开发

需要 Rust 1.96.0、Visual Studio 的 **使用 C++ 的桌面开发** 工作负载，以及 Windows 10/11 SDK。打包需要 .NET SDK 和 WiX 7。

```powershell
. ./scripts/Enter-DevEnvironment.ps1
cargo run --bin SystemMonitorX
```

`Enter-DevEnvironment.ps1` 自动查找含完整 x64 C++ 运行库的 Visual Studio 安装，避免误选未安装 C++ 工作负载的新版本。

```powershell
./scripts/Verify.ps1
./scripts/Package.ps1
```

`Verify.ps1` 运行格式、Clippy、测试及真实系统诊断。诊断只对自己的专用测试子进程做优先级/结束任务验证，不操作用户已有进程。

```powershell
cargo run --bin monitor_probe -- --smoke --verify-operations --output target/smoke-report.json
```

GitHub Actions 在 Windows 上验证并上传 MSI/ZIP。推送 `v*` 标签时自动创建 GitHub Release；依赖版本由 `Cargo.lock` 锁定，动作固定到提交 SHA。

可选的原生 UI 冒烟测试使用实际 Windows 窗口和 D3D11 渲染，检查搜索、暂停、导航、弹窗及最小尺寸。输出目录应放在仓库之外，截图可能包含本机进程信息：

```powershell
cargo run --features ui-test --bin SystemMonitorX -- --ui-smoke-dir C:/Temp/SystemMonitorX-ui-check
```

## 项目结构

```text
src/ui/                 GPUI Kit 原生界面、虚拟表格和交互
src/monitor.rs          CPU/进程/内存/卷/网络采集与速率计算
src/gpu.rs              DXGI / D3DKMT / PDH GPU 采集
src/platform.rs         Windows 服务、启动项和进程管理
src/export.rs           CSV 编码与公式注入防护
src/bin/monitor_probe.rs 无界面诊断和受控操作验证
packaging/              WiX 安装器
scripts/                环境、验证及打包
```

## 技术来源

- [Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit)：原 gpui-component，原生控件、虚拟表格、图表与主题。
- [sysinfo](https://docs.rs/sysinfo/0.39.6/sysinfo/)：CPU、内存、进程、磁盘和网络枚举。
- [Microsoft GPU 性能统计说明](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/)：GPU 引擎和显存计数的含义。
- [Rust for Windows](https://github.com/microsoft/windows-rs)：Win32/DXGI 类型绑定。
- [WiX Toolset](https://docs.firegiant.com/wix/)：MSI 安装器。

Apache-2.0，详见 [LICENSE](LICENSE)。

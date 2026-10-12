# 用 PowerShell 下载、安装和更新

[English](installation.md) · 简体中文

SystemMonitorX 提供预编译的 Windows x64 MSI 和便携 ZIP。适用 Windows 10 1803（内部版本 17134）及以上、Windows 11。安装脚本兼容系统自带 Windows PowerShell 5.1，也可使用 PowerShell 7。

默认安装到当前用户的 `LocalAppData\Programs\SystemMonitorX`，创建开始菜单入口。安装后的路径从 HKCU64 注册表与实际 EXE 文件版本读取。重复运行脚本会获取官方最新稳定版；同版本会显示实际读取到的版本并跳过，使用 `-Reinstall` 可显式重装。

## 快速安装

先从项目官方地址保存脚本，阅读后执行：

~~~powershell
Invoke-WebRequest -UseBasicParsing -Uri 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1' -OutFile '.\Install-SystemMonitorX.ps1'
& '.\Install-SystemMonitorX.ps1'
~~~

已保存的脚本可直接运行这一行完成安装或更新：

~~~powershell
& '.\Install-SystemMonitorX.ps1'
~~~

确认信任项目官方脚本后，也可直接下载并在当前 PowerShell 会话执行：

~~~powershell
& ([ScriptBlock]::Create((irm 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1'))) -Mirror Auto
~~~

参数放在最后，例如追加 `-Version v0.4.0` 指定版本，或 `-DownloadOnly` 只下载。追加 `-SelfTest` 仅运行离线脚本测试，不查询发布元数据、不下载包、不安装。需要先检查脚本内容时，使用上方保存并阅读后执行的方式。

保存为 `.ps1` 文件后的执行遵循当前 PowerShell 执行策略。脚本不会更改执行策略、系统代理、防火墙、证书信任或全局 TLS 配置。企业管理的电脑应按管理员允许的方式运行脚本。

可先预览选中的官方版本、SHA-256 和目标目录：

~~~powershell
& '.\Install-SystemMonitorX.ps1' -WhatIf
~~~

`-WhatIf` 仅查询公开元数据及必要的官方校验值，不下载安装包、不创建目录、不启动 Windows Installer。`-Confirm` 使用 PowerShell 标准确认机制。

## 常用选项

| 参数 | 行为 |
| --- | --- |
| `-Version 0.4.0` 或 `-Version v0.4.0` | 从官方 API 指定稳定版本；省略时使用 GitHub latest 稳定版 |
| `-InstallDirectory 'D:\Apps\SystemMonitorX'` | 指定新 MSI 安装目录；已有 MSI 安装使用原目录，改变目录需先卸载 |
| `-Reinstall` | 对已经安装的同版本重新运行 MSI；不允许降低已装版本 |
| `-Portable` | 选择便携 ZIP，解压到新目录，不创建 MSI 卸载信息或开始菜单 |
| `-DownloadOnly` | 仅保存并验证选中的安装包；即使已安装同版本，仍执行下载和校验 |
| `-DownloadDirectory 'D:\Downloads'` | 仅下载文件的保存目录；默认当前用户 `Downloads\SystemMonitorX` |
| `-Mirror Auto` | 官方优先，失败后顺序尝试明确的公共镜像允许列表 |
| `-Mirror None` | 仅使用官方资产下载地址 |
| `-Mirror GHProxy` | 明确允许当前 GH-Proxy 备用列表；同样优先官方 |
| `-Proxy 'http://127.0.0.1:7890'` | 为此次调用使用用户自己的本机 HTTP/HTTPS 代理；不接受账户密码或远程代理地址 |
| `-HeaderTimeoutSec 10` | 单个请求等待响应头的上限，默认 10 秒 |
| `-DownloadTimeoutSec 180` | 单个候选来源整个下载的上限，默认 180 秒 |
| `-SelfTest` | 离线运行本地 URI、摘要、路径、归档和文件保护测试；本轮临时文件自动清理 |

例如，只从官方地址下载最新 MSI：

~~~powershell
& '.\Install-SystemMonitorX.ps1' -DownloadOnly -Mirror None -DownloadDirectory 'D:\Downloads'
~~~

只下载指定版本的便携 ZIP：

~~~powershell
& '.\Install-SystemMonitorX.ps1' -Version v0.4.0 -Portable -DownloadOnly -Mirror None
~~~

将便携版放入一个新目录：

~~~powershell
& '.\Install-SystemMonitorX.ps1' -Portable -InstallDirectory 'D:\Apps\SystemMonitorX-Portable'
~~~

便携版默认目录按版本区分：`LocalAppData\Programs\SystemMonitorX-Portable\<版本>`。已存在的目标目录会被拒绝，避免覆盖用户文件或运行中的程序。更新便携版时选择新目录，检查新版后自行处理旧目录。

MSI 升级保留原安装路径。脚本不会结束应用进程；请先保存工作并自行关闭 SystemMonitorX。Windows Installer 使用基本界面，FilesInUse 问题由其界面处理，且禁用 Restart Manager 自动关闭进程。脚本接受退出码 `0` 和 `3010`；`3010` 表示需要重启，脚本不会自动重启。返回 `InstalledVerified` 前还会核实安装目录和实际 EXE 版本；待重启的替换则显示 `RestartRequired` 与当时真实读到的版本。

MSI 日志保留在本轮私有临时目录的 `install.log`，结束时会输出路径。下载包会清理；日志包含用于定位安装错误的本机路径，请按需保留。

## 镜像选择与信任来源

默认顺序为：

1. 官方 `github.com` release 资产地址及 GitHub 官方 release CDN。
2. `https://gh-proxy.org/<公开官方资产 URL>`。
3. `https://gh-proxy.com/<公开官方资产 URL>`。

这是**顺序可用性回退**，不是并行测速，也不承诺某个镜像永远最快。每个候选先受响应头超时限制，传输中超过 15 秒无数据或超过总时限会失败。只有完整下载、体积匹配且官方 SHA-256 匹配，才会采用该候选；输出实际采用的来源和耗时。镜像服务可能不可用，`-Mirror None` 可完全关闭公共镜像回退。

这些前缀来自服务提供方自己的[公开下载语法文档](https://gh-proxy.com/docs/github-accelerator)，不是 GitHub 的官方服务。镜像只会收到固定仓库的**公开 release 资产 URL**；脚本不读取 `GH_TOKEN`、`GITHUB_TOKEN` 或任何私有仓库令牌。元数据和校验值始终通过官方 GitHub HTTPS 获取。

信任链如下：

- 版本与资产清单来自 `api.github.com/repos/neko233-com/SystemMonitorX/releases/latest`，或指定版本的官方 tags API；拒绝 draft、prerelease、不匹配的文件名/URL、重复资产和异常体积。
- 优先使用官方 API 的 `digest: sha256:...`；缺少该字段时，仅从官方 GitHub 下载同一 release 的 `SHA256SUMS.txt`。不会使用镜像提供的校验文件。
- 完整文件先写到随机、仅当前用户与 SYSTEM 可访问的临时目录；下载使用 64 KiB 流式缓冲。单包最多 256 MiB，且必须精确匹配官方资产体积。
- 校验后才进行同目录重命名；安装期间保持已验证 MSI 的只读句柄，拒绝文件被并发写入或删除。中断的 `.part` 文件不会成为安装包。
- 所有请求和重定向都检查 HTTPS、无账户凭据及明确主机允许列表；不关闭 TLS 证书检查，不接受 HTTP 降级或任意第三方重定向。
- 便携 ZIP 只允许预期的平铺应用、文档及 VC 运行库文件；拒绝路径逃逸、符号链接、重复名字、缺少主 EXE/更新助手、过多条目或过大的解压内容。完整解压后才发布新目录。

SHA-256 证明下载字节与官方发布资产一致，不能替代发布者的代码签名或对项目本身的信任。当 API 返回 HTTP 403/429 限流时，脚本会使用官方 releases/latest 的 HEAD 重定向验证最新稳定标签地址，再读取该版本的官方 SHA256SUMS.txt（最多 16 KiB）和官方资产 HEAD 的体积。备用路径仅确认当前最新稳定版；指定其它旧版时会等待 API 恢复，不会猜测其发布状态。任何官方路径均无法可靠提供版本与校验值时，脚本停止；不会转而信任镜像的元数据。

GitHub 官方公开资源 API 可以[无需令牌访问](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)，资产摘要由 [GitHub 计算并提供](https://github.blog/changelog/2025-06-03-releases-now-expose-digests-for-release-assets/)。公共 API 的网络可达性和额度仍可能限制下载；本机代理参数也同时用于官方元数据和资产请求。

## 界面语言与 v0.4.0 验收

v0.4.0 使用 GPUI Kit 0.7.3，支持 zh-CN、en-US、ja-JP、ko-KR、de-DE、fr-FR、es-ES、pt-BR。默认跟随 Windows 界面语言偏好，无法匹配时回退英文；设置 → 界面语言可即时切换并保存。切换时保留原始名称、路径、PID/创建身份、当前查询、精确选择和列宽；缓存反馈与硬件展示可重新翻译，不重复查询硬件。CSV 表头固定英文，可选字段缺失时留空。完整地区格式和可访问性仍在路线图中。

未指定版本时脚本选择实际已发布的最新稳定版。本文 v0.4.0 示例须等该版本实际发布后使用，脚本不会假定未发布标签或猜测发布状态。应用保存的语言设置只控制应用界面。MSI 自有产品与安装条件文案使用英文；Windows Installer 系统消息、UAC 和原生系统对话框遵从 Windows 自身语言设置。

<!-- V0.4_INSTALL_ACCEPTANCE: 仅在实际公开产物安装结果完成后更新。 -->
**v0.4.0 当前状态：**源码测试 187 项通过（库 131 + GUI 源码回归 56），2 项默认忽略。真实 Windows/D3D11 原生 QA **34/34 项通过**，完成 **8 次设置语言切换与 40 张截图（1040×640 逻辑像素、1.5× 缩放）**。人工复核覆盖八语言的进程页和设置页（**16/40 张截图**），其余 24 张由自动原生检查覆盖。公开 MSI/ZIP 与安装升级验收仍**待完成**。历史公开 v0.3.0 已通过 PS5.1 安装、文件/快捷方式与便携核验，不代表 v0.4.0 已安装成功。详见[路线图](../ROADMAP.zh-CN.md)和[应用说明](../README.zh-CN.md)。

## 验证脚本

~~~powershell
& '.\Install-SystemMonitorX.ps1' -SelfTest
& '.\Install-SystemMonitorX.ps1' -DownloadOnly -Mirror None -WhatIf
~~~

`-SelfTest` 不联网、不运行 MSI，不改用户安装目录或系统配置；文件保护测试仅创建本轮自有临时夹具并自动删除。`-DownloadOnly` 可验证完整公开安装包，返回版本、体积、SHA-256、来源和保存路径，方便复查。

安装参数与退出码遵循微软 [Windows Installer 命令行说明](https://learn.microsoft.com/en-us/windows/win32/msi/command-line-options)。

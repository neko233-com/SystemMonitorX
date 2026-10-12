# Download, install, and update with PowerShell

English · [简体中文](installation.zh-CN.md)

SystemMonitorX provides prebuilt Windows x64 MSI and portable ZIP packages. It supports Windows 10 version 1803 (build 17134) or later and Windows 11. The installer script works with the built-in Windows PowerShell 5.1 and with PowerShell 7.

The default installation directory is `LocalAppData\Programs\SystemMonitorX` for the current user, with a Start menu shortcut. The script reads the installed path from the HKCU 64-bit registry view and the version from the actual EXE. Running it again selects the latest official stable release. If that version is already installed, the script reports the observed version and skips installation; use `-Reinstall` to explicitly reinstall it.

## Quick install

Save the script from the official project URL, review it, then run it:

~~~powershell
Invoke-WebRequest -UseBasicParsing -Uri 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1' -OutFile '.\Install-SystemMonitorX.ps1'
& '.\Install-SystemMonitorX.ps1'
~~~

Once saved, this command installs or updates the application:

~~~powershell
& '.\Install-SystemMonitorX.ps1'
~~~

If you trust the official project script, you can also download and execute it directly in the current PowerShell session:

~~~powershell
& ([ScriptBlock]::Create((irm 'https://raw.githubusercontent.com/neko233-com/SystemMonitorX/main/scripts/Install-SystemMonitorX.ps1'))) -Mirror Auto
~~~

Place parameters at the end. For example, append `-Version v0.4.0` to select that version, or `-DownloadOnly` to download without installing. Appending `-SelfTest` runs only the script's offline tests: it does not query release metadata, download a package, or install anything. To inspect the script first, use the save-and-review steps above.

Executing a saved `.ps1` file follows the current PowerShell execution policy. The script does not change execution policy, system proxy settings, the firewall, certificate trust, or global TLS settings. On a managed computer, use the method permitted by your administrator.

Preview the selected official version, SHA-256, and destination:

~~~powershell
& '.\Install-SystemMonitorX.ps1' -WhatIf
~~~

`-WhatIf` reads public metadata and any required official checksums. It does not download a package, create directories, or start Windows Installer. `-Confirm` uses PowerShell's standard confirmation mechanism.

## Common options

| Parameter | Behavior |
| --- | --- |
| `-Version 0.4.0` or `-Version v0.4.0` | Select a stable release through the official API; omit it to use GitHub's latest stable release |
| `-InstallDirectory 'D:\Apps\SystemMonitorX'` | Choose the directory for a new MSI installation; an existing MSI installation keeps its original directory, and changing it requires uninstalling first |
| `-Reinstall` | Run the MSI again for an already installed version; downgrades are refused |
| `-Portable` | Select the portable ZIP and extract it into a new directory, without MSI uninstall registration or a Start menu shortcut |
| `-DownloadOnly` | Save and verify the selected package without installing; downloading and verification still run if that version is already installed |
| `-DownloadDirectory 'D:\Downloads'` | Directory for download-only files; defaults to the current user's `Downloads\SystemMonitorX` |
| `-Mirror Auto` | Try the official source first, then fall back in order to the explicit public mirror allowlist |
| `-Mirror None` | Use only official asset download URLs |
| `-Mirror GHProxy` | Explicitly allow the current GH-Proxy fallback list; the official source still comes first |
| `-Proxy 'http://127.0.0.1:7890'` | Use your own local HTTP/HTTPS proxy for this invocation only; credentials and remote proxy addresses are refused |
| `-HeaderTimeoutSec 10` | Response-header timeout per request; defaults to 10 seconds |
| `-DownloadTimeoutSec 180` | Total download timeout per candidate source; defaults to 180 seconds |
| `-SelfTest` | Run offline URI, digest, path, archive, and file-protection tests; temporary fixtures are cleaned up automatically |

Download the latest MSI from official sources only:

~~~powershell
& '.\Install-SystemMonitorX.ps1' -DownloadOnly -Mirror None -DownloadDirectory 'D:\Downloads'
~~~

Download only the portable ZIP for a specific version:

~~~powershell
& '.\Install-SystemMonitorX.ps1' -Version v0.4.0 -Portable -DownloadOnly -Mirror None
~~~

Extract the portable edition into a new directory:

~~~powershell
& '.\Install-SystemMonitorX.ps1' -Portable -InstallDirectory 'D:\Apps\SystemMonitorX-Portable'
~~~

The default portable directory includes the version: `LocalAppData\Programs\SystemMonitorX-Portable\<version>`. An existing destination is refused to protect user files and running applications. To update the portable edition, select a new directory, check the new version, then handle the old directory yourself.

MSI upgrades keep the original installation path. The script does not terminate application processes; save your work and close SystemMonitorX yourself first. Windows Installer runs with a basic interface and handles FilesInUse through that interface. Restart Manager automatic process shutdown is disabled. The script accepts exit codes `0` and `3010`; `3010` means a restart is required, and the script does not restart the computer. Before returning `InstalledVerified`, it checks the installation path and actual EXE version. If replacement is pending a restart, it reports `RestartRequired` with the version observed at that time.

The MSI log remains at `install.log` in this invocation's private temporary directory, and its path is printed when the script finishes. Downloaded packages are cleaned up. Logs contain local paths useful for diagnosing installation errors; retain them as needed.

## Mirror selection and sources of trust

The default order is:

1. Official `github.com` release asset URLs and GitHub's official release CDN.
2. `https://gh-proxy.org/<public official asset URL>`.
3. `https://gh-proxy.com/<public official asset URL>`.

This is **sequential availability fallback**, not parallel speed testing, and no mirror is guaranteed to remain the fastest. Each candidate has a response-header deadline; transfers fail after 15 seconds without data or after the total timeout. A candidate is accepted only after the complete download matches both the expected size and official SHA-256. The script reports the source used and elapsed time. Mirrors may be unavailable; `-Mirror None` disables public mirror fallback completely.

These prefixes come from the provider's own [public download syntax documentation](https://gh-proxy.com/docs/github-accelerator). They are not GitHub services. A mirror receives only a **public release asset URL** for the fixed repository. The script does not read `GH_TOKEN`, `GITHUB_TOKEN`, or any private repository token. Metadata and checksums always come from official GitHub HTTPS endpoints.

The trust chain is:

- Version and asset metadata come from `api.github.com/repos/neko233-com/SystemMonitorX/releases/latest` or the official tags API for a selected version. Drafts, prereleases, mismatched names/URLs, duplicate assets, and abnormal sizes are refused.
- The script prefers the official API's `digest: sha256:...`. If absent, it downloads `SHA256SUMS.txt` for the same release only from official GitHub sources. Mirror-supplied checksum files are never used.
- The complete file is first written into a random temporary directory accessible only to the current user and SYSTEM. Downloads use a 64 KiB streaming buffer. A package is limited to 256 MiB and must exactly match the official asset size.
- A file is renamed within the same directory only after verification. During installation, the verified MSI remains open with a read handle that denies concurrent writes and deletion. Interrupted `.part` files are never treated as installation packages.
- Every request and redirect is checked for HTTPS, absence of credentials, and an explicit host allowlist. TLS certificate verification remains enabled; HTTP downgrades and arbitrary third-party redirects are refused.
- Portable ZIPs allow only the expected flat application, documentation, and VC runtime files. Path traversal, symlinks, duplicate names, missing main EXE/update helper, excessive entries, and oversized extracted content are refused. The new directory is published only after extraction completes.

SHA-256 confirms that downloaded bytes match the official release asset. It does not replace publisher code signing or trust in the project itself. If the API returns HTTP 403/429 due to rate limiting, the script validates the latest stable tag URL using the official releases/latest HEAD redirect, then reads that version's official SHA256SUMS.txt (at most 16 KiB) and asset size through an official HEAD request. This fallback confirms only the current latest stable release. For another older version, retry after API access becomes available; its release status is never guessed. If no official path can reliably provide the version and checksum, the script stops rather than trusting mirror metadata.

GitHub's public resource API is available [without a token](https://docs.github.com/en/rest/releases/releases#get-the-latest-release), and asset digests are [computed and provided by GitHub](https://github.blog/changelog/2025-06-03-releases-now-expose-digests-for-release-assets/). Network availability and public API quotas can still limit downloads. The local proxy parameter applies to both official metadata and asset requests.

## Application language and v0.4.0 acceptance

v0.4.0 uses GPUI Kit 0.7.3 and provides zh-CN, en-US, ja-JP, ko-KR, de-DE, fr-FR, es-ES and pt-BR. The default Follow system preference resolves Windows UI-language preferences, with English fallback; Settings → Interface language switches immediately and saves the choice. Switching languages preserves raw names, paths, PID/creation identities, current search, stable selection and column widths. Cached feedback and hardware presentation are translated again without repeating the hardware query. CSV headers stay English; missing optional fields stay empty. Full regional formatting and accessibility remain roadmap work.

The script selects the latest published stable release unless you specify a version. Stable v0.4.0 was publicly released on 2026-10-12 and is available for the versioned examples; the script never assumes an unpublished tag or guesses release status. The application's saved language preference controls its UI only. The MSI's own product and installation-condition text is English. Windows Installer system messages, UAC and native OS dialogs use Windows' own language settings.

<!-- V0.4_INSTALL_ACCEPTANCE: actual public v0.4.0 installation verified; automatic Newer E2E is a separate scope. -->
**v0.4.0 status:** 187 source tests passed (131 library + 56 GUI source tests), 2 ignored. Native Windows/D3D11 QA passed **34/34 checks**, with **8 Settings language switches and 40 screenshots at 1040×640 logical pixels, 1.5× scale**. Manual review covered Processes and Settings in all eight languages (**16/40 screenshots**); the remaining 24 were covered by automated native checks. The public [v0.4.0 release](https://github.com/neko233-com/SystemMonitorX/releases/tag/v0.4.0) passed final MSI/ZIP/official-digest validation. PS5.1 latest installation handled official API HTTP 403 through official latest/SHA256SUMS/asset-HEAD fallback, downloaded the MSI from Official GitHub in 96.9 s and returned **InstalledVerified 0.4.0, exit 0, no reboot**. The existing v0.3 installation path was retained. Read-only installed acceptance passed **32/32 checks and 16/16 file SHA-256/length matches**, including both EXE versions, HKCU64 InstallPath, actual HKLM64 ARP version/ProductCode and Start menu targets. The production installed copy created a real native window.

The authenticated public ZIP passed all 16 hashes after extraction into a path containing Chinese, Japanese and accented Latin characters. Background hidden startup created a native window observed with EnumWindows and closed normally via owned WM_CLOSE. This verifies startup/window creation, **not portable interaction QA**. The production update-metadata probe parsed the official v0.4.0 MSI name/URL/SHA-256/size; complete automatic Newer/one-click upgrade and cancellation/failure recovery remain roadmap work. [Source CI](https://github.com/neko233-com/SystemMonitorX/actions/runs/38240913272) and [tag CI](https://github.com/neko233-com/SystemMonitorX/actions/runs/38240938655) passed for this release. See the [roadmap](../ROADMAP.md) and [application guide](../README.md).

## Validate the script

~~~powershell
& '.\Install-SystemMonitorX.ps1' -SelfTest
& '.\Install-SystemMonitorX.ps1' -DownloadOnly -Mirror None -WhatIf
~~~

`-SelfTest` does not access the network, run an MSI, or change user installation directories or system configuration. File-protection tests create only temporary fixtures owned by this invocation and remove them automatically. `-DownloadOnly` verifies the complete public package and returns its version, size, SHA-256, source, and saved path for review.

Installation parameters and exit codes follow Microsoft's [Windows Installer command-line documentation](https://learn.microsoft.com/en-us/windows/win32/msi/command-line-options).

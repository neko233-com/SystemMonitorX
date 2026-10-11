$ErrorActionPreference = 'Stop'
# Select an installed C++ toolchain with actual runtime libraries. A newer VS
# installation can be discoverable while its optional C++ workload is absent.
$taskVsWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not (Test-Path -LiteralPath $taskVsWhere)) { throw 'Install Visual Studio with Desktop development with C++.' }
$taskInstalls = & $taskVsWhere -all -products '*' -format json | ConvertFrom-Json
$taskVc = $null
foreach ($taskInstall in ($taskInstalls | Sort-Object installationVersion -Descending)) {
    $taskVcRoot = Join-Path $taskInstall.installationPath 'VC/Tools/MSVC'
    if (-not (Test-Path -LiteralPath $taskVcRoot)) { continue }
    foreach ($taskVersion in (Get-ChildItem -LiteralPath $taskVcRoot -Directory | Sort-Object Name -Descending)) {
        if (Test-Path -LiteralPath (Join-Path $taskVersion.FullName 'lib/x64/msvcrt.lib')) {
            $taskVc = $taskVersion.FullName
            $env:VSINSTALLDIR = $taskInstall.installationPath
            break
        }
    }
    if ($taskVc) { break }
}
if (-not $taskVc) { throw 'No complete MSVC x64 toolchain found.' }
$taskSdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'
$taskSdk = Get-ChildItem -LiteralPath (Join-Path $taskSdkRoot 'Lib') -Directory | Sort-Object Name -Descending | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'um/x64/kernel32.lib') } | Select-Object -First 1
if (-not $taskSdk) { throw 'Windows 10/11 SDK is required.' }
$env:VCToolsInstallDir = "$taskVc/"
$env:WindowsSdkDir = "$taskSdkRoot/"
$env:WindowsSDKVersion = "$($taskSdk.Name)/"
$env:LIB = "$taskVc/lib/x64;$($taskSdk.FullName)/um/x64;$($taskSdk.FullName)/ucrt/x64"
$env:INCLUDE = "$taskVc/include;$taskSdkRoot/Include/$($taskSdk.Name)/ucrt;$taskSdkRoot/Include/$($taskSdk.Name)/shared;$taskSdkRoot/Include/$($taskSdk.Name)/um;$taskSdkRoot/Include/$($taskSdk.Name)/winrt"
$env:PATH = "$taskVc/bin/Hostx64/x64;$taskSdkRoot/bin/$($taskSdk.Name)/x64;$env:PATH"
$env:CC = 'cl.exe'
$env:CXX = 'cl.exe'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = "$taskVc/bin/Hostx64/x64/link.exe"

[CmdletBinding()]
param(
    [string]$Version,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location -LiteralPath $taskRoot
$taskStage = $null
try {
    . (Join-Path $PSScriptRoot 'Enter-DevEnvironment.ps1')
    if (-not $Version) {
        $taskVersionLine = Get-Content -LiteralPath (Join-Path $taskRoot 'Cargo.toml') | Where-Object { $_ -match '^version\s*=\s*"([^"]+)"' } | Select-Object -First 1
        if (-not $taskVersionLine -or $taskVersionLine -notmatch '^version\s*=\s*"([^"]+)"') { throw 'Cannot read the version from Cargo.toml.' }
        $Version = $Matches[1]
    }
    if ($Version -notmatch '^(\d{1,3})\.(\d{1,3})\.(\d{1,5})$' -or [int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or [int]$Matches[3] -gt 65535) {
        throw 'MSI requires a three-part numeric version with major/minor <=255 and patch <=65535.'
    }
    if (-not $SkipBuild) {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw "Release build failed (exit $LASTEXITCODE)." }
    }
    if (-not (Get-Command wix -ErrorAction SilentlyContinue)) { throw 'Install WiX 7: dotnet tool install --global wix --version 7.0.0' }
    $taskWixVersion = (& wix --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $taskWixVersion -notmatch '^7\.') { throw "WiX 7 is required; found '$taskWixVersion'." }
    $taskExe = Join-Path $taskRoot 'target/release/SystemMonitorX.exe'
    foreach ($taskRequired in @($taskExe, (Join-Path $taskRoot 'LICENSE'), (Join-Path $taskRoot 'README.md'), (Join-Path $taskRoot 'assets/app.ico'))) {
        if (-not (Test-Path -LiteralPath $taskRequired -PathType Leaf)) { throw "Required packaging input is missing: $taskRequired" }
    }

    $taskTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $taskStage = Join-Path $taskTempRoot ('SystemMonitorX-package-' + [guid]::NewGuid().ToString('N'))
    $taskPayload = Join-Path $taskStage 'payload'
    New-Item -ItemType Directory -Path $taskPayload -Force | Out-Null
    Copy-Item -LiteralPath $taskExe -Destination $taskPayload
    Copy-Item -LiteralPath (Join-Path $taskRoot 'LICENSE'), (Join-Path $taskRoot 'README.md') -Destination $taskPayload

    # Import inspection decides whether app-local Microsoft VC runtime files
    # are needed. Use the matching installed x64 redistributable directory.
    $taskDependencies = & dumpbin.exe /dependents $taskExe
    if ($LASTEXITCODE -ne 0) { throw 'Unable to inspect EXE dependencies with dumpbin.' }
    $taskRuntimeImports = @($taskDependencies | ForEach-Object {
        if ($_ -match '^\s*((?:vcruntime|msvcp|concrt)[A-Za-z0-9_]*\.dll)\s*$') { $Matches[1] }
    })
    $taskRuntimeFiles = @()
    if ($taskRuntimeImports.Count -gt 0) {
        $taskRedistRoot = Join-Path $env:VSINSTALLDIR 'VC/Redist/MSVC'
        $taskRuntimeDirectory = $null
        foreach ($taskRedistVersion in (Get-ChildItem -LiteralPath $taskRedistRoot -Directory | Where-Object Name -match '^\d' | Sort-Object Name -Descending)) {
            $taskX64 = Join-Path $taskRedistVersion.FullName 'x64'
            if (-not (Test-Path -LiteralPath $taskX64)) { continue }
            foreach ($taskCandidate in (Get-ChildItem -LiteralPath $taskX64 -Directory -Filter 'Microsoft.VC*.CRT')) {
                if (@($taskRuntimeImports | Where-Object { -not (Test-Path -LiteralPath (Join-Path $taskCandidate.FullName $_)) }).Count -eq 0) {
                    $taskRuntimeDirectory = $taskCandidate.FullName
                    break
                }
            }
            if ($taskRuntimeDirectory) { break }
        }
        if (-not $taskRuntimeDirectory) { throw "The x64 Microsoft VC redistributable directory is missing required imports: $($taskRuntimeImports -join ', ')." }
        Copy-Item -LiteralPath (Get-ChildItem -LiteralPath $taskRuntimeDirectory -File -Filter '*.dll').FullName -Destination $taskPayload
        $taskRuntimeFiles = @(Get-ChildItem -LiteralPath $taskPayload -File -Filter '*.dll')
        Write-Host "Included app-local VC runtime from $taskRuntimeDirectory"
    }

    $taskDist = Join-Path $taskRoot 'dist'
    New-Item -ItemType Directory -Path $taskDist -Force | Out-Null
    $taskMsi = Join-Path $taskDist "SystemMonitorX-$Version-x64.msi"
    $taskZip = Join-Path $taskDist "SystemMonitorX-$Version-x64-portable.zip"
    $taskWixArgs = @('build', (Join-Path $taskRoot 'packaging/installer.wxs'), '-arch', 'x64', '-d', "Version=$Version", '-d', "SourceRoot=$taskRoot", '-d', "PayloadDir=$taskPayload", '-pdbtype', 'none', '-o', $taskMsi)
    if ($taskRuntimeFiles.Count -gt 0) {
        $taskRuntimeSource = Join-Path $taskStage 'runtime.wxs'
        $taskRuntimeComponents = foreach ($taskFile in $taskRuntimeFiles) {
            $taskId = 'Runtime_' + ($taskFile.Name -replace '[^A-Za-z0-9_]', '_')
            $taskXmlPath = [Security.SecurityElement]::Escape($taskFile.FullName)
            $taskGuidBytes = [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes("SystemMonitorX:x64:$taskId"))
            $taskGuid = [guid]::new([byte[]]$taskGuidBytes[0..15]).ToString('B').ToUpperInvariant()
            "<Component Id=`"$taskId`" Guid=`"$taskGuid`"><File Source=`"$taskXmlPath`"/><RegistryValue Root=`"HKCU`" Key=`"Software\neko233-com\SystemMonitorX`" Name=`"$taskId`" Type=`"integer`" Value=`"1`" KeyPath=`"yes`"/></Component>"
        }
        $taskRuntimeReferences = foreach ($taskFile in $taskRuntimeFiles) {
            $taskId = 'Runtime_' + ($taskFile.Name -replace '[^A-Za-z0-9_]', '_')
            "<ComponentRef Id=`"$taskId`"/>"
        }
        $taskRuntimeXml = '<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs"><Fragment><DirectoryRef Id="INSTALLFOLDER">' + ($taskRuntimeComponents -join '') + '</DirectoryRef><ComponentGroup Id="RuntimeFiles">' + ($taskRuntimeReferences -join '') + '</ComponentGroup></Fragment></Wix>'
        [IO.File]::WriteAllText($taskRuntimeSource, $taskRuntimeXml, [Text.UTF8Encoding]::new($false))
        $taskWixArgs += @('-d', 'IncludeRuntime=1', $taskRuntimeSource)
    }
    & wix @taskWixArgs
    if ($LASTEXITCODE -ne 0) { throw "MSI build failed (exit $LASTEXITCODE)." }
    & (Join-Path $PSScriptRoot 'Verify.ps1') -PackageOnly -MsiPath $taskMsi -Version $Version -PayloadDirectory $taskPayload

    if (Test-Path -LiteralPath $taskZip) { Remove-Item -LiteralPath $taskZip -Force }
    [IO.Compression.ZipFile]::CreateFromDirectory($taskPayload, $taskZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    $taskArchive = [IO.Compression.ZipFile]::OpenRead($taskZip)
    try {
        $taskZipEntries = @($taskArchive.Entries | ForEach-Object FullName | Sort-Object)
        $taskExpectedEntries = @(Get-ChildItem -LiteralPath $taskPayload -File | ForEach-Object Name | Sort-Object)
        if (Compare-Object $taskExpectedEntries $taskZipEntries) { throw 'Portable ZIP contents do not match the verified payload.' }
    } finally { $taskArchive.Dispose() }
    $taskHashes = foreach ($taskFile in @($taskMsi, $taskZip)) {
        "{0}  {1}" -f (Get-FileHash -LiteralPath $taskFile -Algorithm SHA256).Hash.ToLowerInvariant(), (Split-Path $taskFile -Leaf)
    }
    [IO.File]::WriteAllLines((Join-Path $taskDist 'SHA256SUMS.txt'), $taskHashes, [Text.UTF8Encoding]::new($false))
    Write-Host "Created $taskMsi"
    Write-Host "Created $taskZip"
    Write-Host 'Package content checks and SHA-256 checksums passed.'
} finally {
    if ($taskStage -and (Test-Path -LiteralPath $taskStage)) {
        $taskResolvedStage = [IO.Path]::GetFullPath($taskStage)
        if (-not $taskResolvedStage.StartsWith($taskTempRoot, [StringComparison]::OrdinalIgnoreCase) -or (Split-Path $taskResolvedStage -Leaf) -notlike 'SystemMonitorX-package-*') {
            throw 'Refusing to remove an unexpected package staging directory.'
        }
        Remove-Item -LiteralPath $taskResolvedStage -Recurse -Force
    }
    Pop-Location
}

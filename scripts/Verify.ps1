[CmdletBinding()]
param(
    [switch]$PackageOnly,
    [string]$MsiPath,
    [string]$Version,
    [string]$PayloadDirectory
)

$ErrorActionPreference = 'Stop'
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))

function Assert-ExitCode([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed (exit $LASTEXITCODE)." }
}

function Read-MsiRows($Database, [string]$Sql, [int]$Columns) {
    $taskView = $Database.OpenView($Sql)
    try {
        [void]$taskView.Execute()
        while ($taskRecord = $taskView.Fetch()) {
            $taskRow = for ($taskColumn = 1; $taskColumn -le $Columns; $taskColumn++) {
                $taskRecord.StringData($taskColumn)
            }
            Write-Output -NoEnumerate @($taskRow)
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($taskRecord)
        }
    } finally {
        [void]$taskView.Close()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($taskView)
    }
}

Push-Location -LiteralPath $taskRoot
try {
    if (-not $PackageOnly) {
        . (Join-Path $PSScriptRoot 'Enter-DevEnvironment.ps1')
        & cargo fmt --all -- --check
        Assert-ExitCode 'Rust formatting'
        & cargo clippy --all-targets --locked -- -D warnings
        Assert-ExitCode 'Clippy'
        & cargo test --all-targets --locked
        Assert-ExitCode 'Rust tests'
        & cargo build --release --locked
        Assert-ExitCode 'Release build'
        $taskExe = Join-Path $taskRoot 'target/release/monitor_probe.exe'
        $taskSmokeOut = Join-Path $taskRoot 'target/smoke-report.json'
        $taskSmokeErr = Join-Path $taskRoot 'target/smoke-stderr.txt'
        $taskSmoke = Start-Process -FilePath $taskExe -ArgumentList @('--smoke', '--verify-operations') -Wait -PassThru -WindowStyle Hidden -RedirectStandardOutput $taskSmokeOut -RedirectStandardError $taskSmokeErr
        if ($taskSmoke.ExitCode -ne 0) {
            if (Test-Path -LiteralPath $taskSmokeErr) { Get-Content -LiteralPath $taskSmokeErr | Write-Host }
            throw "Hardware smoke test failed (exit $($taskSmoke.ExitCode))."
        }
        $null = Get-Content -LiteralPath $taskSmokeOut -Raw | ConvertFrom-Json
        Write-Host "Rust checks and hardware smoke test passed. Report: $taskSmokeOut"
    }

    if ($MsiPath) {
        $taskMsi = (Resolve-Path -LiteralPath $MsiPath).Path
        if (-not $Version) { throw 'Version is required when verifying an MSI.' }
        if (-not $PayloadDirectory) { throw 'PayloadDirectory is required when verifying an MSI.' }
        $taskPayload = (Resolve-Path -LiteralPath $PayloadDirectory).Path
        # ICE91 describes per-machine use of per-user directories. This package
        # deliberately has a fixed per-user scope, verified below.
        & wix msi validate $taskMsi -sice ICE91
        Assert-ExitCode 'MSI ICE validation'

        $taskInstaller = New-Object -ComObject WindowsInstaller.Installer
        $taskDatabase = $null
        try {
            $taskDatabase = $taskInstaller.OpenDatabase($taskMsi, 0)
            $taskProperties = @{}
            foreach ($taskRow in (Read-MsiRows $taskDatabase 'SELECT `Property`, `Value` FROM `Property`' 2)) {
                $taskProperties[$taskRow[0]] = $taskRow[1]
            }
            if ($taskProperties['ProductVersion'] -ne $Version) { throw 'MSI ProductVersion does not match the requested version.' }
            if ($taskProperties['ProductName'] -ne 'SystemMonitorX') { throw 'MSI ProductName is incorrect.' }
            if ($taskProperties['UpgradeCode'] -ne '{9D4A61F2-384D-4FA8-901A-340E906D97E8}') { throw 'MSI UpgradeCode changed unexpectedly.' }
            if ($taskProperties['ALLUSERS']) { throw 'MSI must install only for the current user.' }
            $taskDirectories = @{}
            foreach ($taskRow in (Read-MsiRows $taskDatabase 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)) {
                $taskDirectories[$taskRow[0]] = $taskRow
            }
            if ($taskDirectories['INSTALLFOLDER'][1] -ne 'UserProgramsFolder' -or $taskDirectories['UserProgramsFolder'][1] -ne 'LocalAppDataFolder') {
                throw 'MSI destination must be LocalAppData/Programs/SystemMonitorX.'
            }
            $taskFiles = @(Read-MsiRows $taskDatabase 'SELECT `FileName` FROM `File`' 1 | ForEach-Object { ($_[0] -split '\|')[-1] })
            foreach ($taskRequired in @('SystemMonitorX.exe', 'LICENSE', 'README.md')) {
                if ($taskRequired -notin $taskFiles) { throw "MSI is missing $taskRequired." }
            }
            $taskShortcuts = @(Read-MsiRows $taskDatabase 'SELECT `Shortcut`, `Target` FROM `Shortcut`' 2)
            if (-not ($taskShortcuts | Where-Object { $_[0] -eq 'LaunchShortcut' -and $_[1] -eq '[#MainExecutable]' })) { throw 'MSI launch shortcut is missing or invalid.' }
            if (-not ($taskShortcuts | Where-Object { $_[0] -eq 'UninstallShortcut' -and $_[1] -eq '[System64Folder]msiexec.exe' })) { throw 'MSI uninstall shortcut is missing or invalid.' }
        } finally {
            if ($taskDatabase) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($taskDatabase) }
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($taskInstaller)
        }

        # Administrative extraction reads the package without registering or
        # installing the application in the user's profile.
        $taskTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
        $taskExtract = Join-Path $taskTempRoot ('SystemMonitorX-verify-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $taskExtract | Out-Null
        try {
            $taskLog = Join-Path $taskExtract 'extract.log'
            $taskArguments = @('/a', "`"$taskMsi`"", '/qn', "TARGETDIR=`"$taskExtract`"", 'REBOOT=ReallySuppress', '/l*v', "`"$taskLog`"")
            $taskExtraction = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/msiexec.exe') -ArgumentList $taskArguments -Wait -PassThru -WindowStyle Hidden
            if ($taskExtraction.ExitCode -notin @(0, 3010)) {
                Get-Content -LiteralPath $taskLog -Tail 30 | Write-Host
                throw "MSI extraction failed (exit $($taskExtraction.ExitCode))."
            }
            foreach ($taskFile in (Get-ChildItem -LiteralPath $taskPayload -File)) {
                $taskExtracted = @(Get-ChildItem -LiteralPath $taskExtract -Recurse -File | Where-Object Name -eq $taskFile.Name)
                if ($taskExtracted.Count -ne 1) { throw "MSI extraction did not contain exactly one $($taskFile.Name)." }
                if ((Get-FileHash -LiteralPath $taskFile.FullName -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $taskExtracted[0].FullName -Algorithm SHA256).Hash) {
                    throw "MSI extraction hash mismatch: $($taskFile.Name)."
                }
            }
            Write-Host 'MSI properties, shortcuts, per-user destination and all extracted payload hashes passed.'
        } finally {
            $taskResolvedExtract = [IO.Path]::GetFullPath($taskExtract)
            if (-not $taskResolvedExtract.StartsWith($taskTempRoot, [StringComparison]::OrdinalIgnoreCase) -or (Split-Path $taskResolvedExtract -Leaf) -notlike 'SystemMonitorX-verify-*') {
                throw 'Refusing to remove an unexpected extraction directory.'
            }
            Remove-Item -LiteralPath $taskResolvedExtract -Recurse -Force
        }
    } elseif ($PackageOnly) {
        throw 'PackageOnly requires MsiPath.'
    }
} finally {
    Pop-Location
}

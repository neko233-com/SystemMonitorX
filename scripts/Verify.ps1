[CmdletBinding()]
param(
    [switch]$PackageOnly,
    [string]$MsiPath,
    [string]$Version,
    [string]$PayloadDirectory,
    [string]$ZipPath
)

$ErrorActionPreference = 'Stop'
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$taskRequiredPayloadFiles = @('SystemMonitorX.exe', 'SystemMonitorXUpdate.exe', 'LICENSE', 'README.md', 'README.zh-CN.md', 'Install-SystemMonitorX.ps1')

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

function Set-MsiSessionProperty($Session, [string]$Name, [string]$Value) {
    [void]$Session.GetType().InvokeMember('Property', [Reflection.BindingFlags]::SetProperty, $null, $Session, @($Name, $Value))
}

Push-Location -LiteralPath $taskRoot
try {
    if (-not $PackageOnly) {
        . (Join-Path $PSScriptRoot 'Enter-DevEnvironment.ps1')
        & cargo fmt --all -- --check
        Assert-ExitCode 'Rust formatting'
        # Python is a development-only lexical coverage gate; no Python ships.
        $taskPython = Get-Command python -ErrorAction SilentlyContinue
        if (-not $taskPython) { throw 'Python 3 is required for UI translation verification.' }
        & $taskPython.Source (Join-Path $PSScriptRoot 'verify-ui-localization.py') --self-test
        Assert-ExitCode 'UI translation verifier self-tests'
        & $taskPython.Source (Join-Path $PSScriptRoot 'verify-ui-localization.py') --check
        Assert-ExitCode 'Eight-language UI coverage'

        $taskInstallSelfTest = & (Join-Path $PSScriptRoot 'Install-SystemMonitorX.ps1') -SelfTest
        if ($taskInstallSelfTest.Status -ne 'SelfTestPassed' -or $taskInstallSelfTest.Checks -lt 16) {
            throw 'PowerShell installer self-tests did not pass.'
        }
        Write-Host "PowerShell installer self-tests passed ($($taskInstallSelfTest.Checks) checks)."
        & cargo clippy --all-targets --locked --features ui-test -- -D warnings
        Assert-ExitCode 'Clippy'
        & cargo test --all-targets --locked --features ui-test
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
        foreach ($taskRequired in $taskRequiredPayloadFiles) {
            if (-not (Test-Path -LiteralPath (Join-Path $taskPayload $taskRequired) -PathType Leaf)) {
                throw "Required package payload input is missing: $taskRequired."
            }
        }
        # ICE91 describes per-machine use of per-user directories. This package
        # deliberately has a fixed per-user scope, verified below.
        & wix msi validate $taskMsi -sice ICE91
        Assert-ExitCode 'MSI ICE validation'

        $taskInstaller = New-Object -ComObject WindowsInstaller.Installer
        $taskDatabase = $null
        $taskSession = $null
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
            foreach ($taskRequired in $taskRequiredPayloadFiles) {
                if ($taskRequired -notin $taskFiles) { throw "MSI is missing $taskRequired." }
            }

            $taskUpdaterComponents = @(Read-MsiRows $taskDatabase "SELECT ``ComponentId``, ``Directory_``, ``Attributes``, ``KeyPath`` FROM ``Component`` WHERE ``Component`` = 'UpdaterFiles'" 4)
            if ($taskUpdaterComponents.Count -ne 1 -or $taskUpdaterComponents[0][0] -ne '{58A1C636-5115-4405-BF48-763B9D2B0462}' -or $taskUpdaterComponents[0][1] -ne 'INSTALLFOLDER' -or $taskUpdaterComponents[0][3] -ne 'RegistryInstallPath') {
                throw 'MSI detached updater component must have its stable identity, installation directory and registry key path.'
            }
            $taskUpdaterAttributes = [int]$taskUpdaterComponents[0][2]
            # msidbComponentAttributes64bit=256, RegistryKeyPath=4.
            if (($taskUpdaterAttributes -band 256) -eq 0 -or ($taskUpdaterAttributes -band 4) -eq 0) {
                throw 'MSI updater component must use the native 64-bit registry view and a registry key path.'
            }
            $taskUpdaterFiles = @(Read-MsiRows $taskDatabase "SELECT ``Component_``, ``FileName`` FROM ``File`` WHERE ``File`` = 'UpdaterExecutable'" 2)
            if ($taskUpdaterFiles.Count -ne 1 -or $taskUpdaterFiles[0][0] -ne 'UpdaterFiles' -or ($taskUpdaterFiles[0][1] -split '\|')[-1] -ne 'SystemMonitorXUpdate.exe') {
                throw 'MSI updater executable must belong to the independent updater component.'
            }
            $taskInstallPathRows = @(Read-MsiRows $taskDatabase "SELECT ``Root``, ``Key``, ``Name``, ``Value``, ``Component_`` FROM ``Registry`` WHERE ``Registry`` = 'RegistryInstallPath'" 5)
            if ($taskInstallPathRows.Count -ne 1 -or $taskInstallPathRows[0][0] -ne '1' -or $taskInstallPathRows[0][1] -ne 'Software\neko233-com\SystemMonitorX' -or $taskInstallPathRows[0][2] -ne 'InstallPath' -or $taskInstallPathRows[0][3] -ne '[INSTALLFOLDER]' -or $taskInstallPathRows[0][4] -ne 'UpdaterFiles') {
                throw 'MSI RegistryInstallPath must write HKCU64 Software\neko233-com\SystemMonitorX InstallPath as REG_SZ [INSTALLFOLDER].'
            }
            $taskShortcuts = @(Read-MsiRows $taskDatabase 'SELECT `Shortcut`, `Target` FROM `Shortcut`' 2)
            if (-not ($taskShortcuts | Where-Object { $_[0] -eq 'LaunchShortcut' -and $_[1] -eq '[#MainExecutable]' })) { throw 'MSI launch shortcut is missing or invalid.' }
            if (-not ($taskShortcuts | Where-Object { $_[0] -eq 'UninstallShortcut' -and $_[1] -eq '[System64Folder]msiexec.exe' })) { throw 'MSI uninstall shortcut is missing or invalid.' }

            $taskBuildSearch = @(Read-MsiRows $taskDatabase "SELECT ``Signature_`` FROM ``AppSearch`` WHERE ``Property`` = 'WINDOWSBUILDNUMBER'" 1)
            if ($taskBuildSearch.Count -ne 1 -or $taskBuildSearch[0][0] -ne 'WindowsBuildNumberSearch') { throw 'MSI is missing the Windows build AppSearch property.' }
            $taskBuildLocator = @(Read-MsiRows $taskDatabase "SELECT ``Root``, ``Key``, ``Name``, ``Type`` FROM ``RegLocator`` WHERE ``Signature_`` = 'WindowsBuildNumberSearch'" 4)
            # RegLocator raw value (2) + native 64-bit registry flag (16).
            if ($taskBuildLocator.Count -ne 1 -or $taskBuildLocator[0][0] -ne '2' -or $taskBuildLocator[0][1] -ne 'SOFTWARE\Microsoft\Windows NT\CurrentVersion' -or $taskBuildLocator[0][2] -ne 'CurrentBuildNumber' -or $taskBuildLocator[0][3] -ne '18') {
                throw 'MSI Windows build search must read HKLM CurrentBuildNumber from the 64-bit registry.'
            }
            if ('WINDOWSBUILDNUMBER' -notin ($taskProperties['SecureCustomProperties'] -split ';')) { throw 'MSI Windows build property must be secure.' }
            $taskOsCondition = 'Installed OR (WINDOWSBUILDNUMBER >= 17134)'
            $taskLaunchConditions = @(Read-MsiRows $taskDatabase 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2)
            if (-not ($taskLaunchConditions | Where-Object { $_[0] -eq $taskOsCondition -and $_[1] -match '1803.*17134' })) {
                throw 'MSI is missing the Windows 10 1803+ launch condition and uninstall exception.'
            }
            foreach ($taskSequenceTable in @('InstallExecuteSequence', 'InstallUISequence')) {
                $taskSequence = @{}
                foreach ($taskRow in (Read-MsiRows $taskDatabase "SELECT ``Action``, ``Sequence`` FROM ``$taskSequenceTable``" 2)) { $taskSequence[$taskRow[0]] = [int]$taskRow[1] }
                if (-not $taskSequence.ContainsKey('AppSearch') -or -not $taskSequence.ContainsKey('LaunchConditions') -or $taskSequence['AppSearch'] -ge $taskSequence['LaunchConditions']) {
                    throw "MSI must read the Windows build before enforcing launch conditions in $taskSequenceTable."
                }
            }

            # OpenPackage option 1 creates a safe session that prevents changes
            # to the machine. AppSearch only reads registry values; no install
            # actions or Execute/Install sequence is started here.
            $taskInstaller.UILevel = 2
            $taskSession = $taskInstaller.OpenPackage($taskMsi, 1)
            [void]$taskSession.DoAction('AppSearch')
            $taskSearchedBuild = $taskSession.Property('WINDOWSBUILDNUMBER')
            $taskRegistry = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
            $taskWindowsKey = $null
            try {
                $taskWindowsKey = $taskRegistry.OpenSubKey('SOFTWARE\Microsoft\Windows NT\CurrentVersion')
                if (-not $taskWindowsKey) { throw 'Cannot read the native Windows build registry key.' }
                $taskCurrentBuild = [string]$taskWindowsKey.GetValue('CurrentBuildNumber')
            } finally {
                if ($taskWindowsKey) { $taskWindowsKey.Dispose() }
                $taskRegistry.Dispose()
            }
            if (-not $taskCurrentBuild -or $taskSearchedBuild -ne $taskCurrentBuild) { throw 'MSI AppSearch returned an incorrect current Windows build.' }
            Set-MsiSessionProperty $taskSession 'Installed' ''
            $taskCurrentExpected = if ([int]$taskCurrentBuild -ge 17134) { 1 } else { 0 }
            if ($taskSession.EvaluateCondition($taskOsCondition) -ne $taskCurrentExpected) { throw 'MSI Windows condition evaluated incorrectly for the current machine.' }
            foreach ($taskCase in @(
                @{ Build = ''; Installed = ''; Expected = 0 },
                @{ Build = '17133'; Installed = ''; Expected = 0 },
                @{ Build = '17134'; Installed = ''; Expected = 1 },
                @{ Build = '22000'; Installed = ''; Expected = 1 },
                @{ Build = '9999'; Installed = ''; Expected = 0 },
                @{ Build = '17133'; Installed = '1'; Expected = 1 },
                @{ Build = ''; Installed = '1'; Expected = 1 }
            )) {
                Set-MsiSessionProperty $taskSession 'WINDOWSBUILDNUMBER' $taskCase.Build
                Set-MsiSessionProperty $taskSession 'Installed' $taskCase.Installed
                if ($taskSession.EvaluateCondition($taskOsCondition) -ne $taskCase.Expected) { throw "MSI OS condition failed for build '$($taskCase.Build)', Installed '$($taskCase.Installed)'." }
            }
            Write-Host "MSI Windows build AppSearch returned $taskSearchedBuild; minimum 17134, missing/older builds, Win11 and uninstall condition checks passed."
        } finally {
            if ($taskSession) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($taskSession) }
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
            Write-Host 'MSI properties, shortcuts, per-user destination, detached updater, HKCU64 InstallPath and all extracted payload hashes passed.'
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


    if ($ZipPath) {
        if (-not $PayloadDirectory) { throw 'PayloadDirectory is required when verifying a portable ZIP.' }
        $taskPayload = (Resolve-Path -LiteralPath $PayloadDirectory).Path
        $taskZip = (Resolve-Path -LiteralPath $ZipPath).Path
        $taskArchive = [IO.Compression.ZipFile]::OpenRead($taskZip)
        try {
            $taskZipEntries = @($taskArchive.Entries | ForEach-Object FullName | Sort-Object)
            $taskExpectedEntries = @(Get-ChildItem -LiteralPath $taskPayload -File | ForEach-Object Name | Sort-Object)
            foreach ($taskRequired in $taskRequiredPayloadFiles) {
                if ($taskRequired -notin $taskZipEntries -or $taskRequired -notin $taskExpectedEntries) {
                    throw "Portable ZIP or source payload is missing $taskRequired."
                }
            }
            if (Compare-Object $taskExpectedEntries $taskZipEntries) {
                throw 'Portable ZIP contents do not match the verified payload.'
            }
            foreach ($taskEntry in $taskArchive.Entries) {
                $taskPayloadFile = Join-Path $taskPayload $taskEntry.FullName
                if ($taskEntry.Length -ne (Get-Item -LiteralPath $taskPayloadFile).Length) {
                    throw "Portable ZIP length mismatch: $($taskEntry.FullName)."
                }
                $taskEntryStream = $taskEntry.Open()
                try {
                    $taskZipHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($taskEntryStream))
                } finally {
                    $taskEntryStream.Dispose()
                }
                if ($taskZipHash -ne (Get-FileHash -LiteralPath $taskPayloadFile -Algorithm SHA256).Hash) {
                    throw "Portable ZIP hash mismatch: $($taskEntry.FullName)."
                }
            }
            Write-Host 'Portable ZIP includes the application and detached updater; exact contents, lengths and all payload hashes passed.'
        } finally {
            $taskArchive.Dispose()
        }
    }
} finally {
    Pop-Location
}

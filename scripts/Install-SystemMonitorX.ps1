#requires -Version 5.1
<#
.SYNOPSIS
Downloads, verifies and installs the public stable SystemMonitorX x64 release.
.DESCRIPTION
Official GitHub metadata/hash are mandatory. Mirrors transport public assets
only; no token is read or sent. No ExecutionPolicy, firewall, certificate,
system proxy, or global TLS setting is changed. Existing processes are not killed.
#>
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [string]$Version,
    [string]$InstallDirectory,
    [switch]$Portable,
    [switch]$DownloadOnly,
    [string]$DownloadDirectory,
    [ValidateSet('Auto', 'None', 'GHProxy')][string]$Mirror = 'Auto',
    [uri]$Proxy,
    [switch]$Reinstall,
    [ValidateRange(3, 30)][int]$HeaderTimeoutSec = 10,
    [ValidateRange(15, 600)][int]$DownloadTimeoutSec = 180,
    [switch]$SelfTest
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
$script:Repository = 'neko233-com/SystemMonitorX'
$script:MaxPackageBytes = 256MB
$script:OfficialFallbackUsed = $false
$script:OfficialAssetHosts = @('github.com', 'release-assets.githubusercontent.com',
    'objects.githubusercontent.com', 'github-releases.githubusercontent.com')
# Public release download syntax documented by this provider:
# https://gh-proxy.com/docs/github-accelerator
$script:MirrorHosts = @('gh-proxy.org', 'gh-proxy.com')
Add-Type -AssemblyName System.Net.Http
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Get-NormalPath([string]$Path) {
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($Path)) 'A filesystem path is required.'
    Assert-Condition ($Path -notmatch '["\x00-\x1f]' -and $Path -notmatch '^\\\\[?.]\\') 'Device namespaces/control characters/quotes are not accepted.'
    if (-not [IO.Path]::IsPathRooted($Path)) {
        $location = Get-Location
        Assert-Condition ($location.Provider.Name -eq 'FileSystem') 'Use a filesystem current directory.'
        $Path = Join-Path $location.ProviderPath $Path
    }
    foreach ($component in ($Path -split '[\\/]')) {
        if ($component -ne '.' -and $component -ne '..') {
            Assert-Condition ($component -notmatch '[. ]$') 'Ambiguous trailing dot/space path components are not accepted.'
        }
    }
    $full = [IO.Path]::GetFullPath($Path)
    Assert-Condition ($full.Length -lt 260) 'Use a shorter standard DOS/UNC path (less than 260 UTF-16 characters).'
    # Windows PowerShell/.NET Framework can preserve duplicate separators.
    if ($full.StartsWith('\\')) {
        $full = '\\' + [regex]::Replace($full.Substring(2), '[\\/]+', '\')
    } else { $full = [regex]::Replace($full, '[\\/]+', '\') }
    $root = [IO.Path]::GetPathRoot($full)
    $components = $full.Substring($root.Length).Split('\')
    foreach ($component in $components) {
        Assert-Condition ($component -notmatch '[:*?<>|]' -and $component -notmatch '[. ]$') 'Ambiguous Windows path/alternate stream components are not accepted.'
    }
    if ($full.Length -gt $root.Length) { $full = $full.TrimEnd([char[]]@('\', '/')) }
    return $full
}

function Assert-RequestUri([uri]$Uri, [string[]]$AllowedHosts) {
    Assert-Condition ($Uri.IsAbsoluteUri -and $Uri.Scheme -eq 'https' -and
        [string]::IsNullOrEmpty($Uri.UserInfo) -and $Uri.Port -eq 443 -and
        [string]::IsNullOrEmpty($Uri.Fragment)) 'Only credential-free HTTPS requests on port 443 are accepted.'
    Assert-Condition ($AllowedHosts -contains $Uri.DnsSafeHost) ('Unapproved redirect/request host: ' + $Uri.DnsSafeHost)
}

function New-HttpClient {
    $handler = New-Object Net.Http.HttpClientHandler
    $handler.AllowAutoRedirect = $false
    $handler.UseDefaultCredentials = $false
    $handler.AutomaticDecompression = [Net.DecompressionMethods]::None
    # .NET Framework 4.7.2+ ships with the minimum supported Windows 10 1803.
    $handler.SslProtocols = [Security.Authentication.SslProtocols]::Tls12
    if ($Proxy) {
        Assert-Condition ($Proxy.IsAbsoluteUri -and $Proxy.IsLoopback -and
            @('http', 'https') -contains $Proxy.Scheme -and
            [string]::IsNullOrEmpty($Proxy.UserInfo) -and
            $Proxy.AbsolutePath -eq '/' -and [string]::IsNullOrEmpty($Proxy.Query) -and
            [string]::IsNullOrEmpty($Proxy.Fragment)) 'Proxy must be a credential-free loopback HTTP/HTTPS endpoint.'
        $handler.Proxy = New-Object Net.WebProxy($Proxy)
        $handler.Proxy.Credentials = $null
        $handler.UseProxy = $true
    }
    $client = New-Object Net.Http.HttpClient($handler, $true)
    $client.Timeout = [Threading.Timeout]::InfiniteTimeSpan
    return $client
}

function Read-BoundedHttp(
    [Net.Http.HttpClient]$Client, [uri]$Uri, [string[]]$AllowedHosts,
    [long]$MaxBytes, [long]$ExpectedSize = -1, [string]$Destination,
    [switch]$Api, [switch]$Head
) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $output = $null
    $response = $null
    $request = $null
    $completed = $false
    $token = New-Object Threading.CancellationTokenSource
    try {
        if ($Destination) {
            $output = New-Object IO.FileStream($Destination, [IO.FileMode]::CreateNew,
                [IO.FileAccess]::Write, [IO.FileShare]::Read)
        } else { $output = New-Object IO.MemoryStream }
        for ($redirect = 0; $redirect -le 5; $redirect++) {
            Assert-RequestUri $Uri $AllowedHosts
            $remaining = $DownloadTimeoutSec - $clock.Elapsed.TotalSeconds
            Assert-Condition ($remaining -gt 0) 'HTTP transfer exceeded its total deadline.'
            $token.CancelAfter([int]([Math]::Min($HeaderTimeoutSec, $remaining) * 1000))
            $method = [Net.Http.HttpMethod]::Get
            if ($Head) { $method = [Net.Http.HttpMethod]::Head }
            $request = New-Object Net.Http.HttpRequestMessage($method, $Uri)
            $request.Headers.UserAgent.ParseAdd('SystemMonitorX-Installer/1.0')
            if ($Api) {
                $request.Headers.Accept.ParseAdd('application/vnd.github+json')
                [void]$request.Headers.TryAddWithoutValidation('X-GitHub-Api-Version', '2022-11-28')
            } else { $request.Headers.Accept.ParseAdd('application/octet-stream') }
            $response = $Client.SendAsync($request,
                [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $token.Token).GetAwaiter().GetResult()
            if (@(301, 302, 303, 307, 308) -contains [int]$response.StatusCode) {
                Assert-Condition ($redirect -lt 5 -and $null -ne $response.Headers.Location) 'Invalid/excessive HTTP redirects.'
                $next = New-Object Uri($Uri, $response.Headers.Location)
                Assert-RequestUri $next $AllowedHosts
                $response.Dispose(); $response = $null
                $request.Dispose(); $request = $null
                $Uri = $next
                continue
            }
            if ([int]$response.StatusCode -ne 200) {
                $httpError = New-Object Net.WebException('HTTP ' + [int]$response.StatusCode + ' from ' + $Uri.DnsSafeHost)
                $httpError.Data['HttpStatus'] = [int]$response.StatusCode
                throw $httpError
            }
            if ($Head) {
                $completed = $true
                return [pscustomobject]@{Uri = $Uri; ContentLength = $response.Content.Headers.ContentLength; Bytes = $null; Seconds = $clock.Elapsed.TotalSeconds}
            }
            $length = $response.Content.Headers.ContentLength
            if ($null -ne $length) {
                Assert-Condition ([long]$length -le $MaxBytes) 'HTTP Content-Length exceeds the allowed size.'
                if ($ExpectedSize -ge 0) {
                    Assert-Condition ([long]$length -eq $ExpectedSize) 'HTTP Content-Length differs from official release metadata.'
                }
            }
            $token.CancelAfter([int]([Math]::Max(1, $DownloadTimeoutSec - $clock.Elapsed.TotalSeconds) * 1000))
            $input = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
            try {
                $buffer = New-Object byte[] 65536
                [long]$total = 0
                while ($true) {
                    $remaining = $DownloadTimeoutSec - $clock.Elapsed.TotalSeconds
                    Assert-Condition ($remaining -gt 0) 'HTTP transfer exceeded its total deadline.'
                    $read = $input.ReadAsync($buffer, 0, $buffer.Length, $token.Token)
                    if (-not $read.Wait([int]([Math]::Min(15, $remaining) * 1000))) {
                        $token.Cancel()
                        throw 'HTTP transfer stalled for 15 seconds.'
                    }
                    $count = $read.GetAwaiter().GetResult()
                    if ($count -eq 0) { break }
                    $total += $count
                    Assert-Condition ($total -le $MaxBytes -and ($ExpectedSize -lt 0 -or $total -le $ExpectedSize)) 'HTTP body exceeds the allowed/expected size.'
                    $output.Write($buffer, 0, $count)
                }
                Assert-Condition ($total -gt 0 -and ($ExpectedSize -lt 0 -or $total -eq $ExpectedSize)) 'HTTP body is empty/truncated or differs from official size.'
                $output.Flush()
                $bytes = $null
                if (-not $Destination) { $bytes = $output.ToArray() }
                $completed = $true
                return [pscustomobject]@{Bytes = $bytes; Size = $total; Seconds = $clock.Elapsed.TotalSeconds}
            } finally { $input.Dispose() }
        }
        throw 'HTTP redirect limit exceeded.'
    } finally {
        if ($response) { $response.Dispose() }
        if ($request) { $request.Dispose() }
        if ($output) { $output.Dispose() }
        $token.Dispose()
        if ($Destination -and (Test-Path -LiteralPath $Destination)) {
            # Failed transfers may never be mistaken for a complete asset.
            if (-not $completed) {
                Remove-Item -LiteralPath $Destination -Force
            }
        }
    }
}

function Get-VersionFromReleaseUri([uri]$Uri) {
    $pattern = '^https://github\.com/neko233-com/SystemMonitorX/releases/tag/v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
    Assert-Condition ($Uri.AbsoluteUri -cmatch $pattern) 'Official latest redirect is not an exact stable release URL.'
    return $Matches[1] + '.' + $Matches[2] + '.' + $Matches[3]
}

function Get-OfficialFallbackRelease([Net.Http.HttpClient]$Client, [string]$Requested) {
    # Public GitHub release URLs have a separate quota from anonymous REST API.
    # Latest redirect establishes stable status; mirrors never supply metadata.
    $latestUrl = 'https://github.com/' + $script:Repository + '/releases/latest'
    $latest = Read-BoundedHttp $Client ([uri]$latestUrl) @('github.com') 16KB -Head
    $selected = Get-VersionFromReleaseUri $latest.Uri
    if ($Requested) {
        Assert-Condition ($Requested -ceq $selected) 'Anonymous API is limited; official fallback can confirm only the latest stable release. Omit -Version or retry after quota reset.'
    }
    $tag = 'v' + $selected
    $name = 'SystemMonitorX-' + $selected + '-x64.msi'
    if ($Portable) { $name = 'SystemMonitorX-' + $selected + '-x64-portable.zip' }
    $base = 'https://github.com/' + $script:Repository + '/releases/download/' + $tag + '/'
    $sums = Read-BoundedHttp $Client ([uri]($base + 'SHA256SUMS.txt')) $script:OfficialAssetHosts 16KB
    $utf8 = New-Object Text.UTF8Encoding($false, $true)
    $text = $utf8.GetString($sums.Bytes).TrimStart([char]0xFEFF)
    $hash = Get-DigestFromSums $text $name
    $assetHead = Read-BoundedHttp $Client ([uri]($base + $name)) $script:OfficialAssetHosts $script:MaxPackageBytes -Head
    Assert-Condition ($null -ne $assetHead.ContentLength -and [long]$assetHead.ContentLength -gt 0 -and [long]$assetHead.ContentLength -le $script:MaxPackageBytes) 'Official asset HEAD has no valid Content-Length.'
    $script:OfficialFallbackUsed = $true
    return [pscustomobject]@{
        tag_name = $tag; draft = $false; prerelease = $false
        assets = @([pscustomobject]@{
            name = $name; state = 'uploaded'; size = [long]$assetHead.ContentLength
            digest = 'sha256:' + $hash; browser_download_url = $base + $name
        })
    }
}

function Get-Sha256([IO.Stream]$Stream) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $Stream.Position = 0
        $digest = [BitConverter]::ToString($sha.ComputeHash($Stream)).Replace('-', '').ToLowerInvariant()
        $Stream.Position = 0
        return $digest
    } finally { $sha.Dispose() }
}

function Open-VerifiedFile([string]$Path, [long]$Size, [string]$Sha256) {
    $file = New-Object IO.FileStream($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        Assert-Condition ($file.Length -eq $Size) 'Downloaded file size differs from official release metadata.'
        Assert-Condition ((Get-Sha256 $file) -ceq $Sha256) 'SHA-256 verification failed; this file will not be installed.'
        return $file
    } catch { $file.Dispose(); throw }
}

function Get-Asset($Release, [string]$Name, [long]$Limit) {
    $assets = @($Release.assets | Where-Object { $_.name -ieq $Name })
    Assert-Condition ($assets.Count -eq 1 -and $assets[0].name -ceq $Name) ('Missing/duplicate/case-invalid release asset: ' + $Name)
    $asset = $assets[0]
    Assert-Condition ($asset.state -ceq 'uploaded' -and [long]$asset.size -gt 0 -and [long]$asset.size -le $Limit) 'Asset is incomplete or exceeds the size limit.'
    $expected = 'https://github.com/' + $script:Repository + '/releases/download/' + $Release.tag_name + '/' + $Name
    Assert-Condition ([string]$asset.browser_download_url -ceq $expected) 'Asset URL is outside the expected official release.'
    Assert-RequestUri ([uri]$asset.browser_download_url) @('github.com')
    return $asset
}

function Get-DigestFromMetadata($Asset) {
    $property = $Asset.PSObject.Properties['digest']
    if ($null -eq $property -or [string]::IsNullOrEmpty([string]$property.Value)) { return $null }
    Assert-Condition ([string]$property.Value -cmatch '^sha256:([a-fA-F0-9]{64})$') 'Official asset digest is malformed/unsupported.'
    return $Matches[1].ToLowerInvariant()
}

function Get-DigestFromSums([string]$Text, [string]$Name) {
    $digests = @()
    foreach ($line in ($Text -split '\r?\n')) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        Assert-Condition ($line -match '^([a-fA-F0-9]{64})[ \t]+\*?([^ \t\r\n]+)$') 'Official SHA256SUMS has an invalid line.'
        if ($Matches[2] -ceq $Name) { $digests += $Matches[1].ToLowerInvariant() }
    }
    Assert-Condition ($digests.Count -eq 1) 'Official SHA256SUMS is missing/duplicates the selected asset.'
    return $digests[0]
}

function Get-OfficialDigest([Net.Http.HttpClient]$Client, $Release, $Asset) {
    $digest = Get-DigestFromMetadata $Asset
    if ($digest) {
        $source = 'Official GitHub API asset digest'
        if ($script:OfficialFallbackUsed) { $source = 'Official GitHub SHA256SUMS + asset HEAD (anonymous API quota fallback)' }
        return [pscustomobject]@{Hash = $digest; Source = $source}
    }
    $sums = Get-Asset $Release 'SHA256SUMS.txt' 64KB
    $data = Read-BoundedHttp $Client ([uri]$sums.browser_download_url) $script:OfficialAssetHosts 64KB ([long]$sums.size)
    $sumsDigest = Get-DigestFromMetadata $sums
    if ($sumsDigest) {
        $memory = [IO.MemoryStream]::new([byte[]]$data.Bytes, $false)
        try { Assert-Condition ((Get-Sha256 $memory) -ceq $sumsDigest) 'Official checksum asset digest mismatch.' }
        finally { $memory.Dispose() }
    }
    $utf8 = New-Object Text.UTF8Encoding($false, $true)
    $text = $utf8.GetString($data.Bytes).TrimStart([char]0xFEFF)
    return [pscustomobject]@{Hash = (Get-DigestFromSums $text $Asset.name); Source = 'Official GitHub SHA256SUMS.txt'}
}

function New-PrivateDirectory([string]$Parent, [string]$Prefix) {
    $path = Get-NormalPath (Join-Path $Parent ($Prefix + [guid]::NewGuid().ToString('N')))
    Assert-Condition (-not (Test-Path -LiteralPath $path)) 'Random staging directory already exists.'
    $directory = New-Item -ItemType Directory -Path $path
    try {
        $security = New-Object Security.AccessControl.DirectorySecurity
        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $security.SetOwner($sid)
        $security.SetAccessRuleProtection($true, $false)
        $inherit = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
        foreach ($identity in @($sid, (New-Object Security.Principal.SecurityIdentifier('S-1-5-18')))) {
            $rule = New-Object Security.AccessControl.FileSystemAccessRule(
                $identity, [Security.AccessControl.FileSystemRights]::FullControl,
                $inherit, [Security.AccessControl.PropagationFlags]::None,
                [Security.AccessControl.AccessControlType]::Allow)
            $security.AddAccessRule($rule)
        }
        Set-Acl -LiteralPath $path -AclObject $security
        Assert-Condition (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Staging directory is a reparse point.'
        return $path
    } catch { [IO.Directory]::Delete($path); throw }
}

function Remove-OwnedDirectory([string]$Path, [string]$Parent) {
    if (-not $Path -or -not (Test-Path -LiteralPath $Path)) { return }
    $full = Get-NormalPath $Path
    $parentFull = (Get-NormalPath $Parent).TrimEnd('\') + '\'
    Assert-Condition ($full.StartsWith($parentFull, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $full) -match '^(?:SystemMonitorX-install-|\.SystemMonitorX-stage-)[a-f0-9]{32}$') 'Refusing cleanup outside this invocation staging path.'
    Assert-Condition (((Get-Item -LiteralPath $full -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refusing recursive cleanup through a reparse point.'
    Remove-Item -LiteralPath $full -Recurse -Force
}

function Get-InstalledApplication {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey('Software\neko233-com\SystemMonitorX')
        if (-not $key) { return $null }
        try { $path = [string]$key.GetValue('InstallPath', '') } finally { $key.Dispose() }
    } finally { $base.Dispose() }
    if ([string]::IsNullOrWhiteSpace($path)) { return $null }
    $path = Get-NormalPath $path
    $exe = Join-Path $path 'SystemMonitorX.exe'
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { return $null }
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($exe)
    $versionText = [string]$info.ProductVersion
    Assert-Condition ($versionText -match '^([0-9]+\.[0-9]+\.[0-9]+)(?:\.[0-9]+)?$') 'Installed executable has no readable numeric ProductVersion.'
    return [pscustomobject]@{Path = $path; Version = $Matches[1]; Executable = $exe; Source = 'HKCU64 InstallPath + actual EXE ProductVersion'}
}

function Get-DownloadCandidates([string]$OfficialUrl, [string]$Choice) {
    $candidates = @([pscustomobject]@{Name = 'Official GitHub'; Url = [uri]$OfficialUrl; Hosts = $script:OfficialAssetHosts})
    if ($Choice -ne 'None') {
        foreach ($hostName in $script:MirrorHosts) {
            $candidates += [pscustomobject]@{Name = $hostName; Url = [uri]('https://' + $hostName + '/' + $OfficialUrl); Hosts = @($script:MirrorHosts) + @($script:OfficialAssetHosts)}
        }
    }
    return $candidates
}

function Download-VerifiedAsset([Net.Http.HttpClient]$Client, $Asset, [string]$Hash, [string]$Stage) {
    $final = Get-NormalPath (Join-Path $Stage $Asset.name)
    $errors = @()
    foreach ($candidate in (Get-DownloadCandidates $Asset.browser_download_url $Mirror)) {
        $partial = Get-NormalPath (Join-Path $Stage ([guid]::NewGuid().ToString('N') + '.part'))
        Write-Host ('Downloading via ' + $candidate.Name + ' (official SHA-256 required)...')
        try {
            $data = Read-BoundedHttp $Client $candidate.Url $candidate.Hosts $script:MaxPackageBytes ([long]$Asset.size) $partial
            $guard = Open-VerifiedFile $partial ([long]$Asset.size) $Hash
            $guard.Dispose()
            # Same-directory rename occurs only after size/hash verification.
            [IO.File]::Move($partial, $final)
            Write-Host ('Verified {0} bytes in {1:N1}s using {2}.' -f $data.Size, $data.Seconds, $candidate.Name)
            return [pscustomobject]@{Path = $final; Source = $candidate.Name}
        } catch {
            $errors += $candidate.Name + ': ' + $_.Exception.Message
            Write-Warning ('Candidate failed: ' + $candidate.Name + '; trying the next allowed source.')
        } finally {
            if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial -Force }
        }
    }
    throw ('All allowed sources failed; nothing will be installed. ' + ($errors -join ' | '))
}

function Assert-PortableArchive($Archive) {
    $seen = New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    [long]$total = 0
    Assert-Condition ($Archive.Entries.Count -ge 4 -and $Archive.Entries.Count -le 32) 'Unexpected portable archive entry count.'
    foreach ($entry in $Archive.Entries) {
        $name = [string]$entry.FullName
        Assert-Condition ($name -cmatch '^(?:SystemMonitorX(?:Update)?\.exe|LICENSE|README(?:\.zh-CN)?\.md|Install-SystemMonitorX\.ps1|(?:vcruntime|msvcp|concrt|vccorlib)[A-Za-z0-9_]*\.dll)$') 'Portable ZIP contains an unexpected path/file.'
        Assert-Condition ($seen.Add($name)) 'Portable ZIP has duplicate/case-colliding paths.'
        $unixKind = ([long]$entry.ExternalAttributes -shr 16) -band 0xF000
        Assert-Condition ($unixKind -ne 0xA000 -and ($entry.ExternalAttributes -band 0x400) -eq 0) 'Portable ZIP symlink/reparse entries are not accepted.'
        Assert-Condition ($entry.Length -gt 0 -and $entry.Length -le 64MB) 'Portable ZIP entry exceeds the size limit.'
        $total += $entry.Length
        Assert-Condition ($total -le $script:MaxPackageBytes) 'Portable ZIP uncompressed size exceeds the limit.'
    }
    foreach ($required in @('SystemMonitorX.exe', 'SystemMonitorXUpdate.exe', 'LICENSE', 'README.md')) {
        Assert-Condition ($seen.Contains($required)) ('Portable ZIP is missing ' + $required)
    }
}

function Install-Portable([string]$ArchivePath, [string]$Destination) {
    Assert-Condition (-not (Test-Path -LiteralPath $Destination)) 'Portable destination already exists; choose a new/versioned directory. Existing files are never overwritten.'
    $parent = Split-Path -Parent $Destination
    if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    $stage = New-PrivateDirectory $parent '.SystemMonitorX-stage-'
    try {
        $zip = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
        try {
            Assert-PortableArchive $zip
            foreach ($entry in $zip.Entries) {
                $output = New-Object IO.FileStream((Join-Path $stage $entry.FullName),
                    [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
                $input = $entry.Open()
                try {
                    $buffer = New-Object byte[] 65536
                    [long]$written = 0
                    while (($count = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                        $written += $count
                        Assert-Condition ($written -le $entry.Length -and $written -le 64MB) 'Portable ZIP entry inflates beyond its declared/allowed size.'
                        $output.Write($buffer, 0, $count)
                    }
                    Assert-Condition ($written -eq $entry.Length) 'Portable ZIP entry is truncated.'
                }
                finally { $input.Dispose(); $output.Dispose() }
            }
        } finally { $zip.Dispose() }
        # Same-volume directory rename publishes the complete extraction.
        [IO.Directory]::Move($stage, $Destination)
        $stage = $null
    } finally { if ($stage) { Remove-OwnedDirectory $stage $parent } }
}

function Save-VerifiedDownload([string]$Source, [string]$Name, [long]$Size, [string]$Hash, [string]$Directory) {
    if (-not (Test-Path -LiteralPath $Directory)) { New-Item -ItemType Directory -Path $Directory -Force | Out-Null }
    $final = Get-NormalPath (Join-Path $Directory $Name)
    if (Test-Path -LiteralPath $final) {
        $guard = Open-VerifiedFile $final $Size $Hash
        $guard.Dispose()
        return $final
    }
    $partial = Get-NormalPath (Join-Path $Directory ('.' + [guid]::NewGuid().ToString('N') + '.part'))
    try {
        $output = New-Object IO.FileStream($partial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $input = New-Object IO.FileStream($Source, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try { $input.CopyTo($output) } finally { $input.Dispose(); $output.Dispose() }
        $guard = Open-VerifiedFile $partial $Size $Hash
        $guard.Dispose()
        [IO.File]::Move($partial, $final)
        return $final
    } finally { if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial -Force } }
}

function Invoke-SelfTest {
    $checks = 0
    function Test-Rejected([scriptblock]$Action) {
        $rejected = $false
        try { & $Action | Out-Null } catch { $rejected = $true }
        Assert-Condition $rejected ('Expected unsafe input to be rejected: ' + $Action.ToString())
    }
    $hash = 'a' * 64
    Assert-Condition ((Get-DigestFromMetadata ([pscustomobject]@{digest = 'sha256:' + $hash})) -ceq $hash) 'Digest parser failed.'; $checks++
    Test-Rejected { Get-DigestFromMetadata ([pscustomobject]@{digest = 'sha256:123'}) }; $checks++
    Assert-Condition ((Get-DigestFromSums ($hash + '  asset.zip') 'asset.zip') -ceq $hash) 'Checksum parser failed.'; $checks++
    Test-Rejected { Get-DigestFromSums ($hash + '  asset.zip' + [Environment]::NewLine + $hash + '  asset.zip') 'asset.zip' }; $checks++
    Assert-RequestUri ([uri]'https://release-assets.githubusercontent.com/example?public-signature=1') $script:OfficialAssetHosts; $checks++
    Test-Rejected { Assert-RequestUri ([uri]'http://github.com/file') $script:OfficialAssetHosts }; $checks++
    Test-Rejected { Assert-RequestUri ([uri]'https://github.com.attacker.invalid/file') $script:OfficialAssetHosts }; $checks++
    Test-Rejected { Assert-RequestUri ([uri]'https://token@github.com/file') $script:OfficialAssetHosts }; $checks++
    Test-Rejected { Get-NormalPath '\\?\C:\unsafe' }; $checks++
    Test-Rejected { Get-NormalPath 'C:\Apps\" injected' }; $checks++
    $candidates = @(Get-DownloadCandidates 'https://github.com/neko233-com/SystemMonitorX/releases/download/v1.2.3/test.msi' 'Auto')
    Assert-Condition ($candidates.Count -eq 3 -and $candidates[0].Url.DnsSafeHost -eq 'github.com' -and $candidates[1].Url.DnsSafeHost -eq 'gh-proxy.org') 'Candidate priority is wrong.'; $checks++
    Assert-Condition ((Get-VersionFromReleaseUri ([uri]'https://github.com/neko233-com/SystemMonitorX/releases/tag/v0.2.1')) -ceq '0.2.1') 'Fallback tag parsing failed.'; $checks++
    Test-Rejected { Get-VersionFromReleaseUri ([uri]'https://github.com/other/repo/releases/tag/v0.2.1') }; $checks++
    Test-Rejected { Get-VersionFromReleaseUri ([uri]'https://github.com/neko233-com/SystemMonitorX/releases/tag/v0.2.1?unexpected=1') }; $checks++
    Test-Rejected { Get-VersionFromReleaseUri ([uri]'https://github.com/neko233-com/SystemMonitorX/releases/tag/v0.3.0-beta') }; $checks++
    Test-Rejected { Get-NormalPath 'C:\Apps\name:stream' }; $checks++
    Test-Rejected { Get-NormalPath 'C:\Apps\name.' }; $checks++
    Assert-Condition ((Get-NormalPath 'C:\\Apps\\SystemMonitorX') -ceq 'C:\Apps\SystemMonitorX') 'Duplicate path separators were not normalized.'; $checks++
    $data = [pscustomobject]@{Bytes = [byte[]]@(1, 2, 3)}
    $memory = [IO.MemoryStream]::new([byte[]]$data.Bytes, $false)
    try { Assert-Condition ((Get-Sha256 $memory) -ceq '039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81') 'Checksum fallback byte hashing failed.'; $checks++ } finally { $memory.Dispose() }
    $tempRoot = Get-NormalPath ([IO.Path]::GetTempPath())
    $stage = New-PrivateDirectory $tempRoot 'SystemMonitorX-install-'
    try {
        $file = Join-Path $stage 'owned.bin'
        [IO.File]::WriteAllBytes($file, [byte[]]@(1, 2, 3))
        $guard = New-Object IO.FileStream($file, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try { $digest = Get-Sha256 $guard } finally { $guard.Dispose() }
        $guard = Open-VerifiedFile $file 3 $digest
        try { Test-Rejected { [IO.File]::WriteAllBytes($file, [byte[]]@(3, 2, 1)) } } finally { $guard.Dispose() }; $checks++
        Test-Rejected { Open-VerifiedFile $file 3 $hash }; $checks++
        $outputDir = Join-Path $stage 'downloads'
        $saved = Save-VerifiedDownload $file 'owned.bin' 3 $digest $outputDir
        [IO.File]::WriteAllBytes($saved, [byte[]]@(9, 9, 9))
        Test-Rejected { Save-VerifiedDownload $file 'owned.bin' 3 $digest $outputDir }; $checks++
        $fakeZip = [pscustomobject]@{Entries = @(
            [pscustomobject]@{FullName = '../SystemMonitorX.exe'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'SystemMonitorXUpdate.exe'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'LICENSE'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'README.md'; Length = 1; ExternalAttributes = 0})}
        Test-Rejected { Assert-PortableArchive $fakeZip }; $checks++
        $validEntries = @(
            [pscustomobject]@{FullName = 'SystemMonitorX.exe'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'SystemMonitorXUpdate.exe'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'LICENSE'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'README.md'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'Install-SystemMonitorX.ps1'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'README.zh-CN.md'; Length = 1; ExternalAttributes = 0},
            [pscustomobject]@{FullName = 'vccorlib140.dll'; Length = 1; ExternalAttributes = 0})
        Assert-PortableArchive ([pscustomobject]@{Entries = $validEntries}); $checks++
        $legacyEntries = @($validEntries | Where-Object { $_.FullName -cne 'Install-SystemMonitorX.ps1' -and $_.FullName -cne 'README.zh-CN.md' })
        Assert-PortableArchive ([pscustomobject]@{Entries = $legacyEntries}); $checks++
        Test-Rejected { Assert-PortableArchive ([pscustomobject]@{Entries = @($validEntries[0], $validEntries[2], $validEntries[3], $validEntries[4])}) }; $checks++
        Test-Rejected { Assert-PortableArchive ([pscustomobject]@{Entries = $validEntries + $validEntries[0]}) }; $checks++
        $symlink = [pscustomobject]@{FullName = 'vcruntime140.dll'; Length = 1; ExternalAttributes = -1610612736}
        Test-Rejected { Assert-PortableArchive ([pscustomobject]@{Entries = $validEntries + $symlink}) }; $checks++
        $existing = Join-Path $stage 'existing-portable'
        New-Item -ItemType Directory -Path $existing | Out-Null
        $marker = Join-Path $existing 'user-owned.txt'
        [IO.File]::WriteAllText($marker, 'preserve me')
        Test-Rejected { Install-Portable 'never-opened.zip' $existing }
        Assert-Condition ([IO.File]::ReadAllText($marker) -ceq 'preserve me') 'Existing portable data was modified.'; $checks++
        return [pscustomobject]@{Status = 'SelfTestPassed'; Checks = $checks}
    } finally { Remove-OwnedDirectory $stage $tempRoot }
}

if ($SelfTest) {
    if ($PSCmdlet.ShouldProcess('Owned temporary fixtures', 'Run installer self-tests')) { Invoke-SelfTest }
    return
}
Assert-Condition ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT -and [Environment]::Is64BitOperatingSystem) 'SystemMonitorX requires x64 Windows.'
$machine = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
try {
    $windowsKey = $machine.OpenSubKey('SOFTWARE\Microsoft\Windows NT\CurrentVersion')
    try { $build = [int]$windowsKey.GetValue('CurrentBuildNumber', '0') } finally { $windowsKey.Dispose() }
} finally { $machine.Dispose() }
Assert-Condition ($build -ge 17134) 'SystemMonitorX requires Windows 10 1803 (build 17134) or Windows 11.'
$client = $null; $stage = $null; $verified = $null; $keepLog = $false; $log = $null
$tempRoot = Get-NormalPath ([IO.Path]::GetTempPath())
try {
    $client = New-HttpClient
    $endpoint = 'https://api.github.com/repos/' + $script:Repository + '/releases/latest'
    if ($Version) {
        Assert-Condition ($Version -cmatch '^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') 'Version must be a stable numeric version such as 0.2.1 or v0.2.1.'
        $requested = $Version.TrimStart('v')
        $endpoint = 'https://api.github.com/repos/' + $script:Repository + '/releases/tags/v' + $requested
    }
    try {
        $metadata = Read-BoundedHttp $client ([uri]$endpoint) @('api.github.com') 2MB -Api
        $utf8 = New-Object Text.UTF8Encoding($false, $true)
        $release = $utf8.GetString($metadata.Bytes) | ConvertFrom-Json
    } catch {
        if (@(403, 429) -notcontains $_.Exception.Data['HttpStatus']) { throw }
        Write-Warning 'Anonymous GitHub API is limited; using only official release redirect/checksum/HEAD endpoints.'
        $requestedFallback = ''
        if ($Version) { $requestedFallback = $requested }
        $release = Get-OfficialFallbackRelease $client $requestedFallback
    }
    Assert-Condition ($release.draft -eq $false -and $release.prerelease -eq $false -and
        $release.tag_name -cmatch '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') 'Only published stable numeric releases are accepted.'
    $selectedVersion = $release.tag_name.Substring(1)
    if ($Version) { Assert-Condition ($selectedVersion -ceq $requested) 'Official API returned a different requested version.' }
    $parts = $selectedVersion.Split('.')
    Assert-Condition ([long]$parts[0] -le 255 -and [long]$parts[1] -le 255 -and [long]$parts[2] -le 65535) 'Release version is outside MSI limits.'
    $name = 'SystemMonitorX-' + $selectedVersion + '-x64.msi'
    if ($Portable) { $name = 'SystemMonitorX-' + $selectedVersion + '-x64-portable.zip' }
    $asset = Get-Asset $release $name $script:MaxPackageBytes
    $digest = Get-OfficialDigest $client $release $asset
    $installed = Get-InstalledApplication
    if ($installed) { Write-Host ('Observed installed EXE version: ' + $installed.Version + ' at ' + $installed.Path) }
    if ($Portable) {
        if (-not $InstallDirectory) {
            $InstallDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) ('Programs\SystemMonitorX-Portable\' + $selectedVersion)
        }
    } elseif (-not $InstallDirectory) {
        if ($installed) { $InstallDirectory = $installed.Path }
        else { $InstallDirectory = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\SystemMonitorX' }
    }
    $InstallDirectory = Get-NormalPath $InstallDirectory
    if (-not $DownloadOnly) { [void](Get-NormalPath (Join-Path $InstallDirectory 'SystemMonitorX.exe')) }
    if (-not $Portable -and -not $DownloadOnly -and $installed) {
        Assert-Condition ([string]::Equals($InstallDirectory, $installed.Path, [StringComparison]::OrdinalIgnoreCase)) 'Changing an existing MSI install directory requires uninstalling it first.'
        Assert-Condition ([version]$installed.Version -le [version]$selectedVersion) 'Refusing to downgrade the installed application.'
        if ($installed.Version -ceq $selectedVersion -and -not $Reinstall) {
            [pscustomobject]@{Status = 'AlreadyInstalled'; Version = $installed.Version; InstalledPath = $installed.Path; Source = $installed.Source}
            return
        }
    }
    if (-not $DownloadDirectory) { $DownloadDirectory = Join-Path ([Environment]::GetFolderPath('UserProfile')) 'Downloads\SystemMonitorX' }
    $DownloadDirectory = Get-NormalPath $DownloadDirectory
    $target = $InstallDirectory
    $action = 'Download, verify official SHA-256 and install for current user'
    if ($Portable) { $action = 'Download, verify official SHA-256 and extract a new portable directory' }
    if ($DownloadOnly) { $target = $DownloadDirectory; $action = 'Download and verify official SHA-256; do not install' }
    Write-Host ('Selected public stable release: ' + $release.tag_name + '; ' + $name)
    Write-Host ('SHA-256 source: ' + $digest.Source + '; ' + $digest.Hash)
    if (-not $PSCmdlet.ShouldProcess($target, $action)) {
        [pscustomobject]@{Status = 'PlannedOnly'; Version = $selectedVersion; Asset = $name; Sha256 = $digest.Hash; Target = $target}
        return
    }
    $stage = New-PrivateDirectory $tempRoot 'SystemMonitorX-install-'
    $download = Download-VerifiedAsset $client $asset $digest.Hash $stage
    $verified = Open-VerifiedFile $download.Path ([long]$asset.size) $digest.Hash
    if ($DownloadOnly) {
        $saved = Save-VerifiedDownload $download.Path $name ([long]$asset.size) $digest.Hash $DownloadDirectory
        [pscustomobject]@{Status = 'DownloadedVerified'; Version = $selectedVersion; Path = $saved; Sha256 = $digest.Hash; Size = [long]$asset.size; Source = $download.Source; DigestSource = $digest.Source}
        return
    }
    if ($Portable) {
        Install-Portable $download.Path $InstallDirectory
        [pscustomobject]@{Status = 'PortableExtractedVerified'; Version = $selectedVersion; InstalledPath = $InstallDirectory; Sha256 = $digest.Hash; Source = $download.Source}
        return
    }
    $log = Get-NormalPath (Join-Path $stage 'install.log')
    $systemDirectory = [Environment]::SystemDirectory
    if (-not [Environment]::Is64BitProcess) { $systemDirectory = Join-Path $env:WINDIR 'Sysnative' }
    $msiexec = Join-Path $systemDirectory 'msiexec.exe'
    Assert-Condition (Test-Path -LiteralPath $msiexec -PathType Leaf) 'System Windows Installer executable is unavailable.'
    $arguments = '/i "' + $download.Path + '" /qb /norestart /L*v "' + $log + '" MSIRESTARTMANAGERCONTROL=Disable INSTALLFOLDER="' + $InstallDirectory + '"'
    # The verified MSI read guard remains open until Windows Installer exits.
    # Disable Restart Manager automatic shutdown; FilesInUse stays with MSI UI.
    $process = Start-Process -FilePath $msiexec -ArgumentList $arguments -WorkingDirectory $stage -Wait -PassThru
    $keepLog = Test-Path -LiteralPath $log -PathType Leaf
    Assert-Condition (@(0, 3010) -contains $process.ExitCode) ('Windows Installer failed/cancelled (exit ' + $process.ExitCode + '); log: ' + $log)
    $actual = Get-InstalledApplication
    if ($process.ExitCode -eq 3010 -and ($null -eq $actual -or $actual.Version -cne $selectedVersion)) {
        $observed = $null
        if ($actual) { $observed = $actual.Version }
        [pscustomobject]@{Status = 'RestartRequired'; RequestedVersion = $selectedVersion; ObservedExeVersion = $observed; InstalledPath = $InstallDirectory; ExitCode = 3010; RestartRequired = $true; InstallerLog = $log}
        return
    }
    Assert-Condition ($null -ne $actual -and $actual.Version -ceq $selectedVersion -and
        [string]::Equals($actual.Path, $InstallDirectory, [StringComparison]::OrdinalIgnoreCase)) 'MSI returned success, but the expected installed EXE version/path could not be verified.'
    [pscustomobject]@{Status = 'InstalledVerified'; Version = $actual.Version; InstalledPath = $actual.Path; Sha256 = $digest.Hash; Source = $download.Source; ExitCode = $process.ExitCode; RestartRequired = ($process.ExitCode -eq 3010); InstallerLog = $log}
} finally {
    if ($verified) { $verified.Dispose() }
    if ($client) { $client.Dispose() }
    if ($stage) {
        if ($log -and (Test-Path -LiteralPath $log -PathType Leaf)) { $keepLog = $true }
        if ($keepLog) {
            Get-ChildItem -LiteralPath $stage -File | Where-Object { $_.Name -ne 'install.log' } | Remove-Item -Force
            Write-Host ('Private installation log retained at ' + (Join-Path $stage 'install.log'))
        } else { Remove-OwnedDirectory $stage $tempRoot }
    }
}

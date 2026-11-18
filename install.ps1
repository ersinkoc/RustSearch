#requires -Version 5.1
<#
.SYNOPSIS
Installs or updates RustSearch for the current Windows user, without elevation.
.DESCRIPTION
Downloads a GitHub release, verifies SHA-256 hashes, and preserves saved indexes.
Use -Archive for an offline ZIP with its adjacent SHA256SUMS.txt release manifest.
Use -NoLaunch to leave the window closed; enabled background indexing still runs.
Use -NoIntegration only for isolated portable/test installations (no registry or shortcuts).
#>
[CmdletBinding()]
param(
    [string]$Release = 'latest',
    [string]$Archive,
    [string]$ChecksumFile,
    [string]$InstallDirectory = (Join-Path $env:LOCALAPPDATA 'Programs\RustSearch'),
    [string]$DataDirectory = (Join-Path $env:LOCALAPPDATA 'RustSearch'),
    [switch]$StartWithWindows,
    [switch]$DesktopShortcut,
    [switch]$NoLaunch,
    [switch]$NoIntegration,
    [switch]$Uninstall
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$appKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\RustSearch'
$markerName = '.rustsearch-install.json'
$utf8 = New-Object System.Text.UTF8Encoding($false)
$checksumCache = @{}

function FullPath([string]$Path) {
    if (-not [IO.Path]::IsPathRooted($Path) -or $Path.StartsWith('\\')) {
        throw 'Use an absolute local path for installation and data directories.'
    }
    return [IO.Path]::GetFullPath($Path).TrimEnd('\')
}
function Assert-PlainPath([string]$Path) {
    $part = $Path
    while ($part) {
        if ((Test-Path -LiteralPath $part) -and
            ((Get-Item -LiteralPath $part -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Reparse points are not supported in installation paths: $part"
        }
        $part = [IO.Path]::GetDirectoryName($part)
    }
}
function Write-Json([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), $utf8)
}
function Read-Json([string]$Path) {
    return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}
function Assert-Owned([string]$Path) {
    Assert-PlainPath $Path
    $marker = Read-Json (Join-Path $Path $markerName)
    if ($marker.product -ne 'RustSearch' -or (FullPath $marker.install_directory) -ne $Path) {
        throw "Refusing to change a directory not owned by this installer: $Path"
    }
    return $marker
}
function Remove-Staging([string]$Path, [string]$Parent) {
    # Every recursive removal is a verified, immediate, uniquely named child.
    $resolved = FullPath $Path
    if ([IO.Path]::GetDirectoryName($resolved) -ne $Parent -or
        [IO.Path]::GetFileName($resolved) -notmatch '^\.rustsearch-(stage|backup)-[a-f0-9]{32}$') {
        throw "Unsafe staging path: $resolved"
    }
    Assert-PlainPath $resolved
    if (Test-Path -LiteralPath $resolved) {
        if (@(Get-ChildItem -LiteralPath $resolved -Recurse -Force | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count) {
            throw "Refusing recursive removal of a directory containing links: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
function Get-RegistryValue([string]$Key, [string]$Name) {
    if (Test-Path $Key) { return (Get-Item $Key).GetValue($Name, $null) }
    return $null
}
function Restore-Run($Value) {
    if ($null -eq $Value) {
        Remove-ItemProperty -Path $runKey -Name RustSearch -ErrorAction SilentlyContinue
    } else {
        New-Item -Path $runKey -Force | Out-Null
        New-ItemProperty -Path $runKey -Name RustSearch -Value $Value -PropertyType String -Force | Out-Null
    }
}
function Read-Frame($Stream) {
    $header = New-Object byte[] 4
    Read-Bytes $Stream $header
    $length = [BitConverter]::ToUInt32($header, 0)
    if ($length -gt 33554432) { throw 'Invalid worker response size.' }
    $body = New-Object byte[] $length
    Read-Bytes $Stream $body
    return [Text.Encoding]::UTF8.GetString($body) | ConvertFrom-Json
}
function Read-Bytes($Stream, [byte[]]$Bytes) {
    $offset = 0
    while ($offset -lt $Bytes.Length) {
        $count = $Stream.Read($Bytes, $offset, $Bytes.Length - $offset)
        if ($count -eq 0) { throw 'Worker disconnected.' }
        $offset += $count
    }
}
function Send-Frame($Stream, $Value) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Value | ConvertTo-Json -Compress))
    $header = [BitConverter]::GetBytes([uint32]$bytes.Length)
    $Stream.Write($header, 0, 4)
    $Stream.Write($bytes, 0, $bytes.Length)
}
function Stop-ProfileWorker {
    $endpointPath = Join-Path $DataDirectory 'endpoint.json'
    if (-not (Test-Path -LiteralPath $endpointPath)) { return $null }
    $endpoint = Read-Json $endpointPath
    $worker = Get-Process -Id $endpoint.pid -ErrorAction SilentlyContinue
    if ($null -eq $worker) { return $null }
    # Do not kill a PID obtained from a file. Authenticate the selected profile,
    # request orderly shutdown and wait on the original process handle instead.
    if ($worker.ProcessName -ne 'rustsearch-desktop') { throw 'Stale worker endpoint: unrelated PID is active.' }
    $workerPath = $worker.Path
    $client = New-Object Net.Sockets.TcpClient
    try {
        $pending = $client.BeginConnect('127.0.0.1', [int]$endpoint.port, $null, $null)
        if (-not $pending.AsyncWaitHandle.WaitOne(3000)) { throw 'Worker connection timed out.' }
        $client.EndConnect($pending)
        $stream = $client.GetStream()
        $stream.ReadTimeout = 5000
        $stream.WriteTimeout = 5000
        Send-Frame $stream @{ token = $endpoint.token }
        if ((Read-Frame $stream) -ne $true) { throw 'Worker authentication failed.' }
        Send-Frame $stream 'Stop'
        $stopDeadline = [DateTime]::UtcNow.AddSeconds(10)
        do {
            # Progress/results may already be queued ahead of the stop reply.
            $reply = Read-Frame $stream
            $runtimeProperty = $reply.PSObject.Properties['Runtime']
            if ($runtimeProperty -and $runtimeProperty.Value.running -eq $false) { break }
            if ([DateTime]::UtcNow -ge $stopDeadline) { throw 'Worker stop acknowledgement timed out.' }
        } while ($true)
        if (-not $worker.WaitForExit(60000)) { throw 'Worker is still flushing. Retry installation after it stops.' }
        return $workerPath
    } finally { $client.Dispose(); $worker.Dispose() }
}
function Start-ProfileWorker([string]$Executable) {
    if ($DataDirectory.Contains('"')) { throw 'Invalid data directory.' }
    $started = Start-Process -FilePath $Executable -ArgumentList @('--worker', '--data-dir', ('"' + $DataDirectory + '"')) -WindowStyle Hidden -PassThru
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($started.HasExited) { throw 'The background worker exited during startup. See worker-error.txt in the saved-data directory.' }
            $endpointPath = Join-Path $DataDirectory 'endpoint.json'
            if (Test-Path -LiteralPath $endpointPath) {
                $endpoint = Read-Json $endpointPath
                if ($endpoint.pid -eq $started.Id) { return }
            }
            Start-Sleep -Milliseconds 100
        }
        throw 'The background worker did not publish its endpoint within 15 seconds.'
    } finally { $started.Dispose() }
}
function Close-AppWindows([string]$Executable) {
    foreach ($process in @(Get-Process -Name rustsearch-desktop -ErrorAction SilentlyContinue)) {
        if ($process.Path -eq $Executable -and $process.MainWindowHandle -ne 0) {
            if (-not $process.CloseMainWindow() -or -not $process.WaitForExit(15000)) {
                throw 'Close the RustSearch window and retry installation.'
            }
        }
        $process.Dispose()
    }
}
function Assert-BinariesUnlocked {
    foreach ($name in @('rustsearch.exe', 'rustsearch-desktop.exe')) {
        $path = Join-Path $InstallDirectory $name
        if (Test-Path -LiteralPath $path) {
            try { $handle = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
            catch { throw "An executable is still in use, possibly by another data profile. Close that instance and retry: $path" }
            $handle.Dispose()
        }
    }
}
function Verify-File([string]$Path, [string]$Manifest, [string]$Name) {
    if (-not $checksumCache.ContainsKey($Manifest)) {
        $entries = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([StringComparer]::Ordinal)
        foreach ($line in [IO.File]::ReadAllLines($Manifest)) {
            if (-not $line) { continue }
            if ($line -notmatch '^([a-fA-F0-9]{64})  (.+)$') { throw 'Invalid checksum manifest.' }
            if ($entries.ContainsKey($Matches[2])) { throw "Ambiguous checksum: $($Matches[2])" }
            $entries.Add($Matches[2], $Matches[1])
        }
        $checksumCache[$Manifest] = $entries
    }
    $entries = $checksumCache[$Manifest]
    if (-not $entries.ContainsKey($Name)) { throw "Missing checksum: $Name" }
    $expected = $entries[$Name]
    $hasher = [Security.Cryptography.SHA256]::Create()
    $file = [IO.File]::OpenRead($Path)
    try { $actual = ([BitConverter]::ToString($hasher.ComputeHash($file))).Replace('-', '') }
    finally { $file.Dispose(); $hasher.Dispose() }
    if ($actual -ne $expected) {
        throw "SHA-256 mismatch: $Name"
    }
}
function Download([string]$Uri, [string]$Path) {
    if (-not $Uri.StartsWith('https://')) { throw 'Only HTTPS downloads are supported.' }
    Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Path -Headers @{ 'User-Agent' = 'RustSearch-installer' } -TimeoutSec 180
}

function Invoke-Installation {
if ([Environment]::OSVersion.Platform -ne 'Win32NT' -or -not [Environment]::Is64BitProcess) {
    throw 'Run this installer in 64-bit Windows PowerShell 5.1 or PowerShell 7.'
}
if ($NoIntegration -and $StartWithWindows) { throw '-StartWithWindows requires Windows integration.' }
$InstallDirectory = FullPath $InstallDirectory
$DataDirectory = FullPath $DataDirectory
$parent = [IO.Path]::GetDirectoryName($InstallDirectory)
if (-not $parent -or $InstallDirectory.Length -le 3 -or $DataDirectory.Length -le 3 -or
    $InstallDirectory -eq $DataDirectory -or
    $DataDirectory.StartsWith($InstallDirectory + '\', [StringComparison]::OrdinalIgnoreCase) -or
    $InstallDirectory.StartsWith($DataDirectory + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Installation and saved-data directories must be separate, non-root directories.'
}
Assert-PlainPath $InstallDirectory
Assert-PlainPath $DataDirectory
$oldMarker = $null
if (Test-Path -LiteralPath $InstallDirectory) {
    $oldMarker = Assert-Owned $InstallDirectory
    if ((FullPath $oldMarker.data_directory) -ne $DataDirectory -or $oldMarker.integration -ne (-not $NoIntegration)) {
        throw 'Reuse the installed data directory and integration mode when updating or uninstalling.'
    }
}
$desktopExe = Join-Path $InstallDirectory 'rustsearch-desktop.exe'
$profilePath = Join-Path $DataDirectory 'background.json'
$oldProfileBytes = if (Test-Path -LiteralPath $profilePath) { [IO.File]::ReadAllBytes($profilePath) } else { $null }
$profile = if ($null -ne $oldProfileBytes) { Read-Json $profilePath } else {
    [pscustomobject]@{ background = $true; paused = $false; auto_index = $true; start_at_login = $false; sizes = $false; mode = 'Accessible'; roots = @(); catalog = $null }
}
$startup = [bool]$profile.start_at_login -or [bool]$StartWithWindows
$oldRun = Get-RegistryValue $runKey 'RustSearch'
$oldApp = if (Test-Path $appKey) { Get-ItemProperty $appKey } else { $null }
if (-not $NoIntegration -and $oldApp -and $oldApp.InstallLocation -ne $InstallDirectory) {
    throw 'An installed copy exists elsewhere. Reuse its installation directory.'
}
$startLink = Join-Path ([Environment]::GetFolderPath('Programs')) 'RustSearch.lnk'
$desktopLink = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) 'RustSearch.lnk'
$links = @($startLink)
if ($DesktopShortcut -or ($oldMarker -and $oldMarker.desktop_shortcut)) { $links += $desktopLink }
$linkBackups = @{}
if (-not $NoIntegration) {
    foreach ($link in $links) {
        $linkBackups[$link] = if (Test-Path -LiteralPath $link) { [IO.File]::ReadAllBytes($link) } else { $null }
    }
}

if ($Uninstall) {
    if (-not $oldMarker) { throw 'No installer-owned installation exists at this location.' }
    Close-AppWindows $desktopExe
    $null = Stop-ProfileWorker
    Assert-BinariesUnlocked
    # Leave data byte-for-byte intact. Disable only the startup command for this installation.
    if (-not $NoIntegration) {
        if ($oldRun -and $oldRun.StartsWith('"' + $desktopExe + '" ', [StringComparison]::OrdinalIgnoreCase)) { Restore-Run $null }
        foreach ($link in $links) {
            if (Test-Path -LiteralPath $link) {
                $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($link)
                if ($shortcut.TargetPath -eq $desktopExe) { Remove-Item -LiteralPath $link -Force }
            }
        }
        if ($oldApp -and $oldApp.InstallLocation -eq $InstallDirectory) { Remove-Item -LiteralPath $appKey -Recurse -Force }
    }
    $trash = Join-Path $parent ('.rustsearch-backup-' + [guid]::NewGuid().ToString('N'))
    $null = Assert-Owned $InstallDirectory
    Move-Item -LiteralPath $InstallDirectory -Destination $trash
    Remove-Staging $trash $parent
    Write-Host "RustSearch uninstalled. Saved indexes and settings retained: $DataDirectory"
    return
}

$stage = Join-Path $parent ('.rustsearch-stage-' + [guid]::NewGuid().ToString('N'))
$backup = Join-Path $parent ('.rustsearch-backup-' + [guid]::NewGuid().ToString('N'))
$stoppedPath = $null
$activated = $false
$committed = $false
New-Item -ItemType Directory -Path $stage -Force | Out-Null
try {
    if (-not $Archive) {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        if ($Release -ne 'latest' -and $Release -notmatch '^v[0-9A-Za-z.+-]+$') { throw 'Invalid release tag.' }
        $api = if ($Release -eq 'latest') { 'latest' } else { 'tags/' + $Release }
        $releaseInfo = Invoke-RestMethod -Uri "https://api.github.com/repos/ersinkoc/RustSearch/releases/$api" -Headers @{ 'User-Agent' = 'RustSearch-installer' } -TimeoutSec 30
        $assets = @($releaseInfo.assets | Where-Object { $_.name -match '^rustsearch-[0-9A-Za-z.+-]+-windows-x64\.zip$' })
        if ($assets.Count -ne 1) { throw 'The release must contain exactly one Windows x64 ZIP.' }
        $Archive = Join-Path $stage $assets[0].name
        $ChecksumFile = Join-Path $stage 'release-SHA256SUMS.txt'
        Download $assets[0].browser_download_url $Archive
        Download "https://github.com/ersinkoc/RustSearch/releases/download/$($releaseInfo.tag_name)/SHA256SUMS.txt" $ChecksumFile
    } else {
        $Archive = (Resolve-Path -LiteralPath $Archive).Path
        if (-not $ChecksumFile) { $ChecksumFile = Join-Path ([IO.Path]::GetDirectoryName($Archive)) 'SHA256SUMS.txt' }
    }
    $ChecksumFile = (Resolve-Path -LiteralPath $ChecksumFile).ProviderPath
    Verify-File $Archive $ChecksumFile ([IO.Path]::GetFileName($Archive))
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $payload = Join-Path $stage 'payload'
    New-Item -ItemType Directory -Path $payload | Out-Null
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $top = $null
        $seen = @{}
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            if ($name -match '(^/|:|(^|/)\.\.?(/|$)|//)' -or $name.Contains([char]0)) { throw 'Unsafe ZIP entry.' }
            $parts = $name.Split('/')
            if ($null -eq $top) { $top = $parts[0] }
            if ($parts[0] -ne $top -or $top -notmatch '^rustsearch-[0-9A-Za-z.+-]+-windows-x64$') { throw 'Invalid package root.' }
            $relative = $name.Substring($top.Length).TrimStart('/')
            if (-not $relative) { continue }
            $destination = [IO.Path]::GetFullPath((Join-Path $payload $relative))
            if (-not $destination.StartsWith($payload + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'ZIP path escapes package.' }
            if ($seen.ContainsKey($destination)) { throw 'Duplicate ZIP entry.' }
            $seen[$destination] = $true
            # Reject Unix symlinks and Windows reparse attributes, including directory entries.
            if (($entry.ExternalAttributes -band 0x400) -or (($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw 'ZIP links are not supported.' }
            if ($name.EndsWith('/')) { New-Item -ItemType Directory -Path $destination -Force | Out-Null; continue }
            New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($destination)) -Force | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destination, $false)
        }
    } finally { $zip.Dispose() }
    $manifest = Join-Path $payload 'SHA256SUMS.txt'
    foreach ($file in @(Get-ChildItem -LiteralPath $payload -Recurse -File)) {
        $relative = $file.FullName.Substring($payload.Length + 1).Replace('\', '/')
        if ($relative -ne 'SHA256SUMS.txt') { Verify-File $file.FullName $manifest $relative }
    }
    foreach ($required in @('rustsearch.exe', 'rustsearch-desktop.exe', 'install.ps1', 'uninstall.ps1', 'build-info.json')) {
        if (-not (Test-Path -LiteralPath (Join-Path $payload $required) -PathType Leaf)) { throw "Incomplete package: $required" }
    }
    $info = Read-Json (Join-Path $payload 'build-info.json')
    if ($info.target -ne 'x86_64-pc-windows-msvc' -or $info.version -ne '0.0.1' -or $info.commit -notmatch '^[a-f0-9]{40}$') { throw 'Unsupported build provenance.' }
    if ($oldMarker -and $oldMarker.build -gt $info.build) { throw 'Refusing to downgrade an installed build.' }
    $cliVersion = & (Join-Path $payload 'rustsearch.exe') --version
    if ($LASTEXITCODE -ne 0 -or $cliVersion -ne "rustsearch $($info.version)") { throw 'CLI smoke check failed.' }
    if (-not (Test-Path -LiteralPath (Join-Path $env:WINDIR 'System32\VCRUNTIME140.dll'))) {
        throw 'Microsoft Visual C++ x64 runtime is required. Install https://aka.ms/vc14/vc_redist.x64.exe and retry.'
    }
    Write-Json (Join-Path $payload $markerName) @{
        product = 'RustSearch'; install_directory = $InstallDirectory; data_directory = $DataDirectory
        integration = (-not $NoIntegration); desktop_shortcut = ($links -contains $desktopLink)
        version = $info.version; build = $info.build; commit = $info.commit
    }
    Close-AppWindows $desktopExe
    $stoppedPath = Stop-ProfileWorker
    if ($stoppedPath) { Close-AppWindows $stoppedPath }
    if ($oldMarker) {
        $null = Assert-Owned $InstallDirectory
        Assert-BinariesUnlocked
        Move-Item -LiteralPath $InstallDirectory -Destination $backup
    }
    Move-Item -LiteralPath $payload -Destination $InstallDirectory
    $activated = $true
    if ($StartWithWindows) {
        New-Item -ItemType Directory -Path $DataDirectory -Force | Out-Null
        $profile.start_at_login = $true
        $tempProfile = Join-Path $DataDirectory ('.install-profile-' + [guid]::NewGuid().ToString('N'))
        Write-Json $tempProfile $profile
        if (Test-Path -LiteralPath $profilePath) { [IO.File]::Replace($tempProfile, $profilePath, $null) }
        else { [IO.File]::Move($tempProfile, $profilePath) }
    }
    if (-not $NoIntegration) {
        if ($startup -and $profile.background) { Restore-Run ('"' + $desktopExe + '" --worker --data-dir "' + $DataDirectory + '"') }
        else { Restore-Run $null }
        $shell = New-Object -ComObject WScript.Shell
        foreach ($link in $links) {
            New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($link)) -Force | Out-Null
            $shortcut = $shell.CreateShortcut($link)
            $shortcut.TargetPath = $desktopExe
            $shortcut.Arguments = '--data-dir "' + $DataDirectory + '"'
            $shortcut.WorkingDirectory = $InstallDirectory
            $shortcut.IconLocation = "$desktopExe,0"
            $shortcut.Description = 'Search files with RustSearch'
            $shortcut.Save()
        }
        New-Item -Path $appKey -Force | Out-Null
        $uninstallCommand = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "' + (Join-Path $InstallDirectory 'uninstall.ps1') + '"'
        $values = @{
            DisplayName = 'RustSearch'; DisplayVersion = $info.version; Publisher = 'ersinkoc'
            InstallLocation = $InstallDirectory; DisplayIcon = "$desktopExe,0"
            UninstallString = $uninstallCommand; QuietUninstallString = $uninstallCommand
            URLInfoAbout = 'https://github.com/ersinkoc/RustSearch'
        }
        foreach ($key in $values.Keys) { New-ItemProperty -Path $appKey -Name $key -Value $values[$key] -PropertyType String -Force | Out-Null }
        foreach ($key in @('NoModify', 'NoRepair')) { New-ItemProperty -Path $appKey -Name $key -Value 1 -PropertyType DWord -Force | Out-Null }
    }
    if ($profile.background -and ($stoppedPath -or $startup -or -not $NoLaunch)) { Start-ProfileWorker $desktopExe }
    $committed = $true
    Write-Host "Installed RustSearch $($info.version) build $($info.build): $InstallDirectory"
    Write-Host "Saved settings and indexes: $DataDirectory"
} catch {
    $failure = $_
    if ($activated) {
        $null = Stop-ProfileWorker
        # Move the failed installation out of the way before restoring the old one.
        $null = Assert-Owned $InstallDirectory
        $failed = Join-Path $stage 'failed-install'
        Move-Item -LiteralPath $InstallDirectory -Destination $failed
    }
    if (Test-Path -LiteralPath $backup) { Move-Item -LiteralPath $backup -Destination $InstallDirectory }
    if ($activated) {
        if ($null -ne $oldProfileBytes) { [IO.File]::WriteAllBytes($profilePath, $oldProfileBytes) }
        elseif (Test-Path -LiteralPath $profilePath) { Remove-Item -LiteralPath $profilePath }
        if (-not $NoIntegration) {
            Restore-Run $oldRun
            if (Test-Path $appKey) { Remove-Item -LiteralPath $appKey -Recurse -Force }
            if ($oldApp) {
                New-Item -Path $appKey -Force | Out-Null
                foreach ($property in $oldApp.PSObject.Properties) {
                    if ($property.Name -notlike 'PS*') {
                        $type = if ($property.Value -is [int]) { 'DWord' } else { 'String' }
                        New-ItemProperty -Path $appKey -Name $property.Name -Value $property.Value -PropertyType $type -Force | Out-Null
                    }
                }
            }
            foreach ($link in $linkBackups.Keys) {
                if ($null -ne $linkBackups[$link]) { [IO.File]::WriteAllBytes($link, $linkBackups[$link]) }
                elseif (Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link }
            }
        }
    }
    if ($stoppedPath -and (Test-Path -LiteralPath $stoppedPath)) { Start-ProfileWorker $stoppedPath }
    throw $failure
} finally {
    Remove-Staging $stage $parent
    if ($committed -and (Test-Path -LiteralPath $backup)) { Remove-Staging $backup $parent }
}
if (-not $NoLaunch) {
    Start-Process -FilePath $desktopExe -ArgumentList @('--data-dir', ('"' + $DataDirectory + '"')) | Out-Null
}
}

$mutexHash = [Security.Cryptography.SHA256]::Create()
try { $mutexName = ([BitConverter]::ToString($mutexHash.ComputeHash([Text.Encoding]::UTF8.GetBytes((FullPath $InstallDirectory).ToUpperInvariant())))).Replace('-', '') }
finally { $mutexHash.Dispose() }
$mutex = New-Object Threading.Mutex($false, ('Local\RustSearchInstall-' + $mutexName))
$acquired = $false
try {
    try { $acquired = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $acquired = $true }
    if (-not $acquired) { throw 'Another RustSearch installation is already in progress.' }
    Invoke-Installation
} finally {
    if ($acquired) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}

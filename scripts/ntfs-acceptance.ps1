param(
    [ValidatePattern('^[A-Za-z]:$')][string]$Volume = 'D:',
    [string]$RunDirectory
)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $workspace 'target\release\rustsearch.exe'
if (-not (Test-Path -LiteralPath $exe)) { throw 'Build release CLI before running acceptance.' }
if (-not $RunDirectory) { $RunDirectory = Join-Path $workspace ('.temp_files\ntfs-' + [guid]::NewGuid().ToString('N')) }
$RunDirectory = [IO.Path]::GetFullPath($RunDirectory)
$allowed = [IO.Path]::GetFullPath((Join-Path $workspace '.temp_files')) + '\'
if (-not $RunDirectory.StartsWith($allowed, [StringComparison]::OrdinalIgnoreCase)) { throw 'Acceptance output must stay under the project .temp_files directory.' }
if (-not $workspace.StartsWith($Volume + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture and target NTFS volume must match.' }
New-Item -ItemType Directory -Path $RunDirectory -Force | Out-Null
$status = Join-Path $RunDirectory 'status.json'
function Write-Status([string]$State, [string]$Message) {
    @{ state=$State; message=$Message; utc=[DateTime]::UtcNow.ToString('o') } | ConvertTo-Json | Set-Content -LiteralPath $status -Encoding utf8
}
function Invoke-Cli([string[]]$CliArgs, [string]$LogName) {
    $out = Join-Path $RunDirectory ($LogName + '.stdout.json')
    $err = Join-Path $RunDirectory ($LogName + '.stderr.log')
    & $exe @CliArgs 1> $out 2> $err
    if ($LASTEXITCODE -ne 0) { throw ('CLI failed: ' + $LogName + ': ' + (Get-Content -LiteralPath $err -Raw)) }
    return (Get-Content -LiteralPath $out -Raw | ConvertFrom-Json)
}
try {
    Write-Status 'running' 'Preparing isolated real-disk fixture'
    $fixture = Join-Path $RunDirectory 'fixture'
    New-Item -ItemType Directory -Path $fixture | Out-Null
    $child = Join-Path $fixture 'child'
    New-Item -ItemType Directory -Path $child | Out-Null
    $token = 'rsprobe-' + [guid]::NewGuid().ToString('N')
    $first = Join-Path $fixture ($token + '-a.txt')
    $deleted = Join-Path $fixture ($token + '-delete.bin')
    $unicode = Join-Path $child ($token + '-İstanbul-Ω.txt')
    [IO.File]::WriteAllBytes($first, [byte[]]::new(4096))
    [IO.File]::WriteAllBytes($deleted, [byte[]]::new(13))
    [IO.File]::WriteAllBytes($unicode, [byte[]]::new(0))
    $snapshot = Join-Path $RunDirectory 'native.rsidx'
    $probe = Invoke-Cli -CliArgs @('probe','--volume',$Volume) -LogName 'probe'
    $machine = @{
        cpu=(Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)
        os=(Get-CimInstance Win32_OperatingSystem | Select-Object Caption,BuildNumber,TotalVisibleMemorySize)
        disk=(Get-Partition -DriveLetter $Volume.Substring(0,1) | Get-Disk | Select-Object FriendlyName,BusType,Size)
        exe_sha256=(Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
        volume=$probe.volume; journal_before=$probe.journal
    }
    $machine | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $RunDirectory 'machine.json') -Encoding utf8
    Write-Status 'running' 'Enumerating native NTFS metadata and sampling 5000 file sizes'
    $index = Invoke-Cli -CliArgs @('index','--volume',$Volume,'--sizes','--size-limit','5000','--output',$snapshot,'--report',(Join-Path $RunDirectory 'index.json')) -LogName 'index'
    $before = Invoke-Cli -CliArgs @('search','--index',$snapshot,'--query',$token,'--json') -LogName 'fixture-before'
    if ($before.total -ne 3) { throw "Expected 3 initial fixture files, got $($before.total)." }
    if ($index.cache.state -ne 'built' -or $before.cache.state -ne 'hit') {throw 'Index preparation did not make the first query a cache hit.'}
    $cacheHash=(Get-FileHash -LiteralPath ($snapshot+'.search') -Algorithm SHA256).Hash
    $baseHash=(Get-FileHash -LiteralPath $snapshot -Algorithm SHA256).Hash
    # Mutations affect only this run's newly-created fixture.
    $renamed = Join-Path $fixture ($token + '-renamed.txt')
    Move-Item -LiteralPath $first -Destination $renamed
    [IO.File]::WriteAllBytes($renamed, [byte[]]::new(8231))
    $moved = Join-Path $fixture ($token + '-İstanbul-Ω.txt')
    Move-Item -LiteralPath $unicode -Destination $moved
    [IO.File]::WriteAllBytes($moved, [byte[]]::new(29))
    Remove-Item -LiteralPath $deleted
    $created = Join-Path $child ($token + '-new.bin')
    [IO.File]::WriteAllBytes($created, [byte[]]::new(0))
    Write-Status 'running' 'Replaying journal and validating against independent directory enumeration'
    $refresh = Invoke-Cli -CliArgs @('refresh','--index',$snapshot,'--sizes','--report',(Join-Path $RunDirectory 'refresh.json')) -LogName 'refresh'
    if ((Get-FileHash -LiteralPath $snapshot -Algorithm SHA256).Hash -ne $baseHash) {throw 'Refresh rewrote the base snapshot.'}
    if (-not (Test-Path -LiteralPath ($snapshot+'.delta'))) {throw 'Refresh did not persist a delta log.'}
    $after = Invoke-Cli -CliArgs @('search','--index',$snapshot,'--query',$token,'--json') -LogName 'fixture-after'
    if ($after.cache.state -ne 'hit' -or $after.cache.bound_sequence -ne 1) {throw 'Delta query did not reuse and rebind the base cache.'}
    if ((Get-FileHash -LiteralPath ($snapshot+'.search') -Algorithm SHA256).Hash -ne $cacheHash) {throw 'Delta update unnecessarily rewrote the base search cache.'}
    $expected = @(Get-ChildItem -LiteralPath $fixture -Recurse -File -Force)
    if ($after.total -ne $expected.Count) { throw "Fixture count mismatch: $($after.total) vs $($expected.Count)." }
    foreach ($file in $expected) {
        $row = @($after.rows | Where-Object { $_.path -eq $file.FullName })
        if ($row.Count -ne 1 -or $row[0].logical_size -ne $file.Length) { throw 'Fixture path or size mismatch after journal replay.' }
    }
    $refreshAgain=Invoke-Cli -CliArgs @('refresh','--index',$snapshot,'--sizes','--report',(Join-Path $RunDirectory 'refresh-again.json')) -LogName 'refresh-again'
    if ($refreshAgain.index.log_sequence -ne 2) {throw 'Second refresh did not replay and advance the delta sequence.'}
    if ((Get-FileHash -LiteralPath $snapshot -Algorithm SHA256).Hash -ne $baseHash) {throw 'Second refresh rewrote the base snapshot.'}
    $afterAgain=Invoke-Cli -CliArgs @('search','--index',$snapshot,'--query',$token,'--json') -LogName 'fixture-after-again'
    if ($afterAgain.cache.state -ne 'hit' -or $afterAgain.cache.bound_sequence -ne 2 -or $afterAgain.total -ne $expected.Count) {throw 'Second delta query did not reuse a correct cache view.'}
    foreach ($file in $expected) {$row=@($afterAgain.rows | Where-Object {$_.path -eq $file.FullName});if($row.Count -ne 1 -or $row[0].logical_size -ne $file.Length){throw 'Second cached delta path/size mismatch'}}
    if ((Get-FileHash -LiteralPath ($snapshot+'.search') -Algorithm SHA256).Hash -ne $cacheHash) {throw 'Second delta rewrote base cache.'}
    $check = @{passed=$true; expected_files=$expected.Count; initial_files=$before.total; checks=@('native enumeration','rename','move','resize','create','delete','UTF-16 path','zero length','journal checkpoint replay','unchanged base SHA256','delta replay across processes','second delta append','persistent search cache hit','delta cache rebind','unchanged search cache SHA256'); native_records=$index.index.records; pending_refresh=$refresh.index.pending_refresh; base_sha256=$baseHash; cache_sha256=$cacheHash; delta_bytes=(Get-Item -LiteralPath ($snapshot+'.delta')).Length}
    $check | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $RunDirectory 'acceptance.json') -Encoding utf8
    Write-Status 'complete' 'Native enumeration and journal fixture acceptance passed'
} catch {
    Write-Status 'failed' $_.Exception.Message
    exit 1
}

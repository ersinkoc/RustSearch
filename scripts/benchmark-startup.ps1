param(
    [Parameter(Mandatory)][string]$Index,
    [Parameter(Mandatory)][string]$BaselineExe,
    [Parameter(Mandatory)][string]$RunDirectory,
    [ValidateRange(3,15)][int]$Trials=5,
    [long]$AffinityMask=1,
    [string]$CurrentIndex
)
$ErrorActionPreference='Stop'
$workspace=Split-Path -Parent $PSScriptRoot
$current=Join-Path $workspace 'target\release\rustsearch.exe'
$Index=(Resolve-Path -LiteralPath $Index).Path
$CurrentIndex=if($CurrentIndex){(Resolve-Path -LiteralPath $CurrentIndex).Path}else{$Index}
$BaselineExe=(Resolve-Path -LiteralPath $BaselineExe).Path
$RunDirectory=[IO.Path]::GetFullPath($RunDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $workspace '.temp_files'))+'\'
if(-not $RunDirectory.StartsWith($allowed,[StringComparison]::OrdinalIgnoreCase)){throw 'Run directory must stay in project .temp_files'}
New-Item -ItemType Directory -Path $RunDirectory -Force | Out-Null
$samples=[Collections.Generic.List[object]]::new()
$sourceHash=(Get-FileHash -LiteralPath $Index -Algorithm SHA256).Hash
$deltaHash=if(Test-Path -LiteralPath ($Index+'.delta')){(Get-FileHash -LiteralPath ($Index+'.delta') -Algorithm SHA256).Hash}else{$null}
if((Get-FileHash -LiteralPath $CurrentIndex -Algorithm SHA256).Hash -ne $sourceHash){throw 'Variant base snapshots must be byte-identical'}
$currentDeltaHash=if(Test-Path -LiteralPath ($CurrentIndex+'.delta')){(Get-FileHash -LiteralPath ($CurrentIndex+'.delta') -Algorithm SHA256).Hash}else{$null}
if($currentDeltaHash -ne $deltaHash){throw 'Variant delta logs must be byte-identical'}
function Invoke-Sample([string]$Variant,[string]$Exe,[int]$Trial) {
    $info=[Diagnostics.ProcessStartInfo]::new()
    $info.FileName=$Exe;$info.WorkingDirectory=$workspace;$info.UseShellExecute=$false;$info.CreateNoWindow=$true
    $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
    $sampleIndex=if($Variant -eq 'baseline'){$Index}else{$CurrentIndex}
    foreach($arg in @('search','--index',$sampleIndex,'--query','__rs_cache_startup_no_match_c73e__','--limit','0','--json')){[void]$info.ArgumentList.Add($arg)}
    $process=[Diagnostics.Process]::new();$process.StartInfo=$info
    $timer=[Diagnostics.Stopwatch]::StartNew();[void]$process.Start()
    try {$process.ProcessorAffinity=[IntPtr]$AffinityMask} catch {$process.Kill();throw 'Could not apply requested child process affinity'}
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit(60000)){$process.Kill();throw 'Startup sample exceeded 60 second deadline'}
    $text=$stdout.GetAwaiter().GetResult();$errorText=$stderr.GetAwaiter().GetResult();$timer.Stop()
    if($process.ExitCode -ne 0){throw "$Variant failed: $errorText"}
    $result=$text | ConvertFrom-Json
    if($result.total -ne 0){throw 'No-match benchmark unexpectedly matched files'}
    if($Variant -eq 'cached' -and $result.cache.state -ne 'hit'){throw 'Cached sample did not reuse the persistent cache'}
    $record=[pscustomobject]@{variant=$Variant;trial=$Trial;external_ms=$timer.Elapsed.TotalMilliseconds;load_ms=$result.load_ms;prepare_ms=$result.prepare_ms;query_ms=$result.query_ms;cache=$result.cache;records=$result.index.records;delta_sequence=$result.index.log_sequence;resources=$result.resources}
    $record | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $RunDirectory ("$Variant-$Trial.json")) -Encoding utf8
    $samples.Add($record);$process.Dispose()
    @{state='running';completed=$samples.Count;last_variant=$Variant;last_trial=$Trial} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDirectory 'startup-status.json') -Encoding utf8
}
try {
    # One unmeasured current invocation creates the derived cache when migrating an old snapshot.
    $warm=& $current search --index $CurrentIndex --query '__rs_cache_startup_no_match_c73e__' --limit 0 --json | Out-String
    if($LASTEXITCODE -ne 0){throw 'Cache warmup failed'}
    $warm | Set-Content -LiteralPath (Join-Path $RunDirectory 'cache-warmup.json') -Encoding utf8
    $cacheHash=(Get-FileHash -LiteralPath ($CurrentIndex+'.search') -Algorithm SHA256).Hash
    $baselineCacheHash=(Get-FileHash -LiteralPath ($Index+'.search') -Algorithm SHA256).Hash
    for($trial=1;$trial -le $Trials;$trial++) {
        if($trial%2 -eq 1){Invoke-Sample 'baseline' $BaselineExe $trial;Invoke-Sample 'cached' $current $trial}
        else {Invoke-Sample 'cached' $current $trial;Invoke-Sample 'baseline' $BaselineExe $trial}
    }
    if((Get-FileHash -LiteralPath $Index -Algorithm SHA256).Hash -ne $sourceHash){throw 'Frozen source changed during benchmark'}
    if((Get-FileHash -LiteralPath ($CurrentIndex+'.search') -Algorithm SHA256).Hash -ne $cacheHash){throw 'Cache hits rewrote the search cache'}
    $afterDelta=if(Test-Path -LiteralPath ($Index+'.delta')){(Get-FileHash -LiteralPath ($Index+'.delta') -Algorithm SHA256).Hash}else{$null}
    if($afterDelta -ne $deltaHash){throw 'Delta log changed during benchmark'}
    if((Get-FileHash -LiteralPath $CurrentIndex -Algorithm SHA256).Hash -ne $sourceHash){throw 'Current source changed'}
    if((Get-FileHash -LiteralPath ($Index+'.search') -Algorithm SHA256).Hash -ne $baselineCacheHash){throw 'Baseline cache changed'}
    $afterCurrentDelta=if(Test-Path -LiteralPath ($CurrentIndex+'.delta')){(Get-FileHash -LiteralPath ($CurrentIndex+'.delta') -Algorithm SHA256).Hash}else{$null}
    if($afterCurrentDelta -ne $deltaHash){throw 'Current delta changed'}
    $recordCount=$samples[0].records
    if(@($samples | Where-Object {$_.records -ne $recordCount}).Count){throw 'Benchmark corpus counts differ'}
    $sequence=$samples[0].delta_sequence
    if(@($samples | Where-Object {$_.delta_sequence -ne $sequence}).Count){throw 'Benchmark delta sequences differ'}
    $summary=@{}
    foreach($variant in @('baseline','cached')) {
        $rows=@($samples | Where-Object {$_.variant -eq $variant});$stats=@{}
        foreach($field in @('external_ms','load_ms','prepare_ms','query_ms')){$values=@($rows | ForEach-Object {$_.$field} | Sort-Object);$stats[$field]=@{median=$values[[int][Math]::Floor($values.Count/2)];min=$values[0];max=$values[-1]}}
        foreach($field in @('private_bytes','peak_private_bytes_process_lifetime','working_set_bytes','cpu_ms')){$values=@($rows | ForEach-Object {$_.resources.$field} | Sort-Object);$stats[$field]=@{median=$values[[int][Math]::Floor($values.Count/2)];min=$values[0];max=$values[-1]}}
        $summary[$variant]=$stats
    }
    @{trials=$Trials;records=$recordCount;delta_sequence=$sequence;delta_sha256=$deltaHash;affinity_mask=$AffinityMask;measurement='new process each trial; alternating variants; fixed child CPU affinity; OS/file cache not flushed; p95/p99 not claimed';baseline_sha256=(Get-FileHash -LiteralPath $BaselineExe -Algorithm SHA256).Hash;current_sha256=(Get-FileHash -LiteralPath $current -Algorithm SHA256).Hash;source_sha256=$sourceHash;baseline_cache_sha256=$baselineCacheHash;current_index=$CurrentIndex;baseline_index=$Index;cache_sha256=$cacheHash;cache_bytes=(Get-Item -LiteralPath ($CurrentIndex+'.search')).Length;summary=$summary;samples=$samples} | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $RunDirectory 'startup-summary.json') -Encoding utf8
    @{state='complete';samples=$samples.Count} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDirectory 'startup-status.json') -Encoding utf8
} catch {@{state='failed';error=$_.Exception.Message} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDirectory 'startup-status.json') -Encoding utf8;throw}

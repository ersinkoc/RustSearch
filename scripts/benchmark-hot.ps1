param(
    [Parameter(Mandatory)][string]$Index,
    [Parameter(Mandatory)][string]$BaselineExe,
    [Parameter(Mandatory)][string]$RunDirectory,
    [ValidateRange(3,15)][int]$Trials=5,
    [long]$AffinityMask=1
)
$ErrorActionPreference='Stop'
$workspace=Split-Path -Parent $PSScriptRoot
$current=Join-Path $workspace 'target\release\rustsearch.exe'
$Index=(Resolve-Path -LiteralPath $Index).Path
$BaselineExe=(Resolve-Path -LiteralPath $BaselineExe).Path
$RunDirectory=[IO.Path]::GetFullPath($RunDirectory)
$allowed=[IO.Path]::GetFullPath((Join-Path $workspace '.temp_files'))+'\'
if(-not $RunDirectory.StartsWith($allowed,[StringComparison]::OrdinalIgnoreCase)){throw 'Output must stay in project .temp_files'}
New-Item -ItemType Directory -Path $RunDirectory -Force | Out-Null
$hashes=@{}
foreach($path in @($Index,($Index+'.search'),($Index+'.delta'))){if(Test-Path -LiteralPath $path){$hashes[$path]=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash}}
$samples=[Collections.Generic.List[object]]::new()
function Invoke-Sample([string]$Variant,[string]$Exe,[int]$Trial) {
    $info=[Diagnostics.ProcessStartInfo]::new()
    $info.FileName=$Exe;$info.WorkingDirectory=$workspace;$info.UseShellExecute=$false;$info.CreateNoWindow=$true
    $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
    foreach($arg in @('bench','--index',$Index,'--iterations','100','--warmup','5','--verify-linear')){[void]$info.ArgumentList.Add($arg)}
    $process=[Diagnostics.Process]::new();$process.StartInfo=$info
    [void]$process.Start()
    try {$process.ProcessorAffinity=[IntPtr]$AffinityMask} catch {$process.Kill();throw 'Could not apply child CPU affinity'}
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit(60000)){$process.Kill();throw 'Hot benchmark exceeded 60 seconds'}
    $body=$stdout.GetAwaiter().GetResult();$errorBody=$stderr.GetAwaiter().GetResult()
    if($process.ExitCode -ne 0){throw "$Variant failed: $errorBody"}
    $result=$body | ConvertFrom-Json
    if(-not $result.verified_linear -or $result.cache.state -ne 'hit' -or $result.cases.Count -ne 9){throw 'Oracle or cache-hit check failed'}
    $body | Set-Content -LiteralPath (Join-Path $RunDirectory "$Variant-$Trial.json") -Encoding utf8
    $samples.Add([pscustomobject]@{variant=$Variant;trial=$Trial;result=$result})
    $process.Dispose()
    @{state='running';samples=$samples.Count} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDirectory 'status.json') -Encoding utf8
}
try {
    for($trial=1;$trial -le $Trials;$trial++) {
        if($trial%2 -eq 1){Invoke-Sample 'baseline' $BaselineExe $trial;Invoke-Sample 'current' $current $trial}
        else {Invoke-Sample 'current' $current $trial;Invoke-Sample 'baseline' $BaselineExe $trial}
    }
    foreach($path in $hashes.Keys){if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $hashes[$path]){throw 'Frozen input changed'}}
    $reference=$samples[0].result
    foreach($sample in $samples) {
        if($sample.result.index.records -ne $reference.index.records -or $sample.result.index.log_sequence -ne $reference.index.log_sequence){throw 'Corpus differs'}
        for($i=0;$i -lt 9;$i++) {
            if($sample.result.cases[$i].total_matches -ne $reference.cases[$i].total_matches){throw 'Case count differs'}
        }
    }
    $summary=@{}
    foreach($variant in @('baseline','current')) {
        $rows=@($samples | Where-Object {$_.variant -eq $variant});$cases=@()
        for($i=0;$i -lt 9;$i++) {
            $values=@($rows | ForEach-Object {$_.result.cases[$i].p50_ms} | Sort-Object)
            $cases+=@{case=$reference.cases[$i].case;median_p50_ms=$values[[int][Math]::Floor($values.Count/2)];min_p50_ms=$values[0];max_p50_ms=$values[-1]}
        }
        $summary[$variant]=$cases
    }
    @{trials=$Trials;affinity_mask=$AffinityMask;records=$reference.index.records;delta_sequence=$reference.index.log_sequence;baseline_sha256=(Get-FileHash -LiteralPath $BaselineExe -Algorithm SHA256).Hash;current_sha256=(Get-FileHash -LiteralPath $current -Algorithm SHA256).Hash;input_hashes=$hashes;summary=$summary} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $RunDirectory 'summary.json') -Encoding utf8
    @{state='complete';samples=$samples.Count} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDirectory 'status.json') -Encoding utf8
} catch {@{state='failed';error=$_.Exception.Message} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $RunDirectory 'status.json') -Encoding utf8;throw}

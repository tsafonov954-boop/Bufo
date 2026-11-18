<#
.SYNOPSIS
  Measures a screen recorder's resource use, so Bufo and Medal (or any other
  recorder) can be compared fairly on the same PC.

.DESCRIPTION
  Samples once per second for -Seconds: working set, private bytes, CPU
  (percent of ONE core) of every process whose name matches -Process, plus
  whole-GPU utilisation / memory from nvidia-smi (NVIDIA only).
  Writes <Label>.csv and prints a summary. Run it while the SAME game or
  workload is on screen for every recorder, with each recorder idle-recording.

.EXAMPLE
  # Bufo: start it, then measure for 2 minutes
  .\bench.ps1 -Label bufo -Process bufo -Start ..\target\release\bufo.exe -Seconds 120
  # Medal (already running):
  .\bench.ps1 -Label medal -Process Medal -Seconds 120
  # Baseline with no recorder at all (GPU/CPU of the game alone):
  .\bench.ps1 -Label baseline -Process none -Seconds 120
#>
param(
    [Parameter(Mandatory)] [string]$Label,
    [Parameter(Mandatory)] [string]$Process,   # process name without .exe; several: "Medal,Medal.Overlay"
    [string]$Start = "",                       # optional exe to launch (and stop afterwards)
    [int]$Seconds = 120,
    [int]$Warmup = 15                          # seconds ignored at the start
)

$names = $Process -split ',' | ForEach-Object { $_.Trim() }
$cores = [Environment]::ProcessorCount
$started = $null
if ($Start) {
    $started = Start-Process $Start -PassThru
    Write-Host "started $Start (pid $($started.Id))"
}

$smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue
function Get-Gpu {
    if (-not $smi) { return @($null, $null) }
    $o = & nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits 2>$null
    if (-not $o) { return @($null, $null) }
    $p = $o.Split(',') | ForEach-Object { [double]$_.Trim() }
    return @($p[0], $p[1])
}
function Get-Procs { if ($names -contains 'none') { @() } else { Get-Process -Name $names -ErrorAction SilentlyContinue } }

$rows = @()
$prev = @{}; $prevT = Get-Date
foreach ($p in Get-Procs) { $prev[$p.Id] = $p.TotalProcessorTime.TotalSeconds }
Write-Host "measuring '$Label' for $Seconds s (first $Warmup s are warm-up)..."
for ($i = 1; $i -le $Seconds; $i++) {
    Start-Sleep -Seconds 1
    $now = Get-Date; $dt = ($now - $prevT).TotalSeconds; $prevT = $now
    $ws = 0.0; $priv = 0.0; $cpu = 0.0; $n = 0
    foreach ($p in Get-Procs) {
        $n++
        $ws += $p.WorkingSet64 / 1MB; $priv += $p.PrivateMemorySize64 / 1MB
        $t = $p.TotalProcessorTime.TotalSeconds
        if ($prev.ContainsKey($p.Id)) { $cpu += ($t - $prev[$p.Id]) / $dt * 100 }
        $prev[$p.Id] = $t
    }
    $g = Get-Gpu
    $rows += [pscustomobject]@{
        t = $i; procs = $n; ws_mb = [math]::Round($ws, 1); private_mb = [math]::Round($priv, 1)
        cpu_pct_one_core = [math]::Round($cpu, 1); gpu_util_pct = $g[0]; gpu_mem_mb = $g[1]
    }
}
if ($started -and -not $started.HasExited) { Stop-Process -Id $started.Id }

$rows | Export-Csv -NoTypeInformation -Path "$Label.csv"
$m = $rows | Where-Object { $_.t -gt $Warmup }
if (-not $m) { $m = $rows }
function Avg($f) { [math]::Round((($m | Measure-Object -Property $f -Average).Average), 1) }
function Max($f) { [math]::Round((($m | Measure-Object -Property $f -Maximum).Maximum), 1) }
Write-Host ""
Write-Host "=== $Label (logical cores: $cores) ==="
Write-Host ("Working set  MB : avg {0}  max {1}" -f (Avg ws_mb), (Max ws_mb))
Write-Host ("Private      MB : avg {0}  max {1}" -f (Avg private_mb), (Max private_mb))
Write-Host ("CPU  % of 1 core: avg {0}  max {1}  (= {2}% of the whole CPU)" -f (Avg cpu_pct_one_core), (Max cpu_pct_one_core), [math]::Round((Avg cpu_pct_one_core) / $cores, 2))
if ($smi) {
    Write-Host ("GPU util %  (whole GPU): avg {0}  max {1}" -f (Avg gpu_util_pct), (Max gpu_util_pct))
    Write-Host ("GPU memory MB (whole GPU): avg {0}  max {1}" -f (Avg gpu_mem_mb), (Max gpu_mem_mb))
} else { Write-Host "nvidia-smi not found: GPU columns are empty." }
Write-Host "raw data: $Label.csv"

# Summarise VICTORY_BENCH_CSV frame logs, one CSV row per log.
#
#   powershell -File scripts\bench-frames.ps1 -Csv a.csv,b.csv [-From 60] [-To 300] [-Out summary.csv]
#
# A frame log (VICTORY_BENCH_CSV=<file>) has one line per frame the layer
# presents: frame, t_s (seconds since the first present), interval_ms (since
# the previous present), unix_s. Frames that frame generation adds are not in
# it. -From / -To keep the frames whose t_s is in that window (seconds after
# the first present), to leave loading out; without them the whole log counts.
#
# Logic taken from VIctory-Stable's bench/analyze.ps1 (Shoqapique, "via
# VIctory-Stable by LeBonVieuxAxel"); the Windows counter columns and the
# anchor folders of its bench/run.ps1 are left out. Definitions:
#   avg_fps         frames in the window / the window's length
#   low_1pct_fps    1000 / the 99th percentile frame time (the 1 % slowest frames)
#   low_0_1pct_fps  1000 / the 99.9th percentile frame time
#   ft_*_ms         frame time: mean, median, 99th and 99.9th percentiles, longest
#   over_50ms, over_100ms, over_200ms  frames longer than that (hitches)
param(
    [Parameter(Mandatory)][string[]]$Csv,
    [double]$From = 0,
    [double]$To = [double]::PositiveInfinity,
    [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
$inv = [Globalization.CultureInfo]::InvariantCulture
# `powershell -File` hands "a.csv,b.csv" over as one string.
$Csv = @($Csv | ForEach-Object { $_ -split ',' } | Where-Object { $_ })

# The p-th percentile of sorted values (nearest rank).
function Get-Pct([double[]]$sorted, [double]$p) {
    if ($sorted.Count -eq 0) { return [double]::NaN }
    $i = [math]::Min($sorted.Count - 1, [math]::Ceiling($p * $sorted.Count) - 1)
    return $sorted[[math]::Max(0, $i)]
}
function F([double]$x, [int]$d = 1) { if ([double]::IsNaN($x) -or [double]::IsInfinity($x)) { '' } else { $x.ToString("F$d", $inv) } }

$result = New-Object System.Collections.Generic.List[string]
$result.Add('log,window_s,frames,avg_fps,low_1pct_fps,low_0_1pct_fps,ft_mean_ms,ft_median_ms,ft_p99_ms,ft_p999_ms,ft_max_ms,over_50ms,over_100ms,over_200ms')
foreach ($file in $Csv) {
    $name = Split-Path -Leaf $file
    if (-not (Test-Path $file)) { $result.Add("$name,missing"); continue }
    $lines = [IO.File]::ReadAllLines((Resolve-Path $file))
    if ($lines.Count -lt 2) { $result.Add("$name,empty"); continue }
    $keys = $lines[0].TrimStart([char]0xFEFF).Split(',')
    $iT = [array]::IndexOf($keys, 't_s')
    $iMs = [array]::IndexOf($keys, 'interval_ms')
    if ($iT -lt 0 -or $iMs -lt 0) { $result.Add("$name,no t_s or interval_ms column"); continue }
    $times = New-Object System.Collections.Generic.List[double]
    $ft = New-Object System.Collections.Generic.List[double]
    # The first line's interval is 0 (no earlier present): it is not a frame time.
    for ($i = 2; $i -lt $lines.Count; $i++) {
        $v = $lines[$i].Split(',')
        if ($v.Count -le [math]::Max($iT, $iMs)) { continue }  # a line cut by a forced stop
        $t = 0.0; $ms = 0.0
        if (-not [double]::TryParse($v[$iT], [Globalization.NumberStyles]::Float, $inv, [ref]$t)) { continue }
        if (-not [double]::TryParse($v[$iMs], [Globalization.NumberStyles]::Float, $inv, [ref]$ms)) { continue }
        if ($t -lt $From -or $t -gt $To) { continue }
        $times.Add($t); $ft.Add($ms)
    }
    $span = if ($times.Count -gt 1) { $times[$times.Count - 1] - $times[0] } else { [double]::NaN }
    $sorted = [double[]]$ft.ToArray(); [array]::Sort($sorted)
    $mean = if ($sorted.Count) { ($sorted | Measure-Object -Average).Average } else { [double]::NaN }
    $max = if ($sorted.Count) { $sorted[-1] } else { [double]::NaN }
    $result.Add((@(
        $name, (F $span), $sorted.Count,
        (F (($times.Count - 1) / $span)), (F (1000 / (Get-Pct $sorted 0.99))), (F (1000 / (Get-Pct $sorted 0.999))),
        (F $mean 2), (F (Get-Pct $sorted 0.5) 2), (F (Get-Pct $sorted 0.99) 2), (F (Get-Pct $sorted 0.999) 2), (F $max 2),
        @($sorted | Where-Object { $_ -gt 50 }).Count, @($sorted | Where-Object { $_ -gt 100 }).Count, @($sorted | Where-Object { $_ -gt 200 }).Count
    ) -join ','))
}
if ($Out) { $result | Out-File -Encoding utf8 $Out }
$result

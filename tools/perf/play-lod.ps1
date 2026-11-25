# Launch GTA VI on branch feature/levels-on-demand (feature/watch-batch with
# origin/main merged in, plus fxceless/victory-fork PR #795): the same settings
# as play-best.ps1, plus levels on demand (only the mip levels the title has
# loaded take card memory).
#   powershell -ExecutionPolicy Bypass -File "G:\GTA VI\tools\play-lod.ps1"
#   ... -NoLod       same build, levels on demand off (A/B against it)
#   ... -Diag        diagnostics build: memory lines every few seconds
# Log: G:\GTA VI\profiles\lod-run.log (lod-off-run.log with -NoLod).
#
# First launch: the shader cache is now keyed on the translator itself
# (upstream), not on VICTORY_SHADER_CACHE_KEY: every shader is translated
# again once (several minutes before the world is whole). Measure on the
# second launch.
# Close OBS, Wallpaper Engine, browsers, Discord and launchers first: at idle
# they held 4.3 GB of the card's 16 GB.
param([switch]$NoLod, [switch]$Diag)

$out = 'G:\GTA VI\profiles'
New-Item -ItemType Directory -Force $out | Out-Null
Set-Location 'G:\GTA VI\victory-gta6'

# Own cache directory (victory-lod-data): the pipeline library of the older
# builds held pipelines under the same names without the depth bounds test,
# and runs that read it lost textures and lighting.
$env:VICTORY_CONFIG = 'G:\GTA VI\victory-gta6\victory-lod.toml'
$env:VICTORY_DISPLAY_MODE = '1920x1080'
$env:VICTORY_RELAY = '2'
$env:VICTORY_LATE_WAIT = '3'
$env:VICTORY_WIDE_BACK = '1'
$env:VICTORY_CARD_BUFFERS_MIN_MB = '1'
$env:VICTORY_CARD_BUFFERS_MB = '1024'

# CPU side (as play-best.ps1).
$env:VICTORY_TABLES_MEMO = '2'
$env:VICTORY_ASYNC_SUBMIT = '1'
$env:VICTORY_FAST_COPY = '1'
$env:VICTORY_ASK_SHARED = '1'
$env:VICTORY_LIST_POOL_MOST = '1024'
$env:VICTORY_LIST_POOL_MB = '2048'

# Card side (as play-best.ps1).
$env:VICTORY_INDIRECT_AMPLIFY = '1'
$env:VICTORY_AMPLIFY_WAIT = 'auto'
$env:VICTORY_CLAMP_WAIT = 'auto'
$env:VICTORY_CARD_REHASH_RUNS = '1'
$env:VICTORY_JOIN_COPIES = '1'

# The submitting thread no longer waits for flights to land before reading
# title memory: the title's bytes are put in beside the flights' writes
# (no settle), and 2 GB of windows (vertex, index and constant data of the
# title's allocator) get a side in the card's memory. Measured 2026-11-25 in
# the opening cutscene: ~6.4 fps -> 7.4-8.1 fps, card 52-59% busy (the frame
# then follows the CPU's preparing). Windows up to 95% of the budget
# (VICTORY_CARD_WINDOWS_PERCENT=95, 4 GB) was worse: 6.8-7.0 fps.
$env:VICTORY_LATE_FILL_AHEAD = '1'
$env:VICTORY_LATE_MERGE_RUNS = '1000000'
$env:VICTORY_LATE_MERGE_GAP = '64'
$env:VICTORY_CARD_WINDOWS_MB = '2048'
# Upstream's guard over the other views of a page off: 6.9-8.6 fps against
# 7.4-8.1, picture checked right in the opening cutscene. It may miss title
# writes through another view of a page elsewhere: remove this line if a
# scene shows stale or missing data.
$env:VICTORY_VIEW_GUARD = '0'

# New: card memory for the mip levels the title holds only. (Upstream's
# VICTORY_CARD_ROOM_FOR_BUFFERS is on by default in this build: the texture
# queue keeps room for the card copies of title buffers.)
if ($NoLod) {
    Remove-Item Env:VICTORY_LEVELS_ON_DEMAND -ErrorAction SilentlyContinue
    $log = "$out\lod-off-run.log"
} else {
    $env:VICTORY_LEVELS_ON_DEMAND = '1'
    $log = "$out\lod-run.log"
}

if ($Diag) {
    $exe = 'G:\GTA VI\victory-gta6\target\diag\release\victory-lod-diag.exe'
} else {
    $exe = 'G:\GTA VI\victory-gta6\target\release\victory-lod8.exe'
}

& $exe run 'G:\GTA VI\GTA61.0.2.1\game_scarlett_master.exe' --execute --console series-x *> $log

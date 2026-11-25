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
# On a 24 GB card (RTX 4090), in the measured 14.96 fps set: 4096 here,
# 6144 for upload windows and 64 KiB copies below, room for buffers.
if ($env:VICTORY_BIG_CARD -eq '1') { $env:VICTORY_CARD_BUFFERS_MB = '4096'; $env:VICTORY_CARD_ROOM_FOR_BUFFERS = '1' }

# CPU side (as play-best.ps1).
$env:VICTORY_TABLES_MEMO = '2'
$env:VICTORY_ASYNC_SUBMIT = '1'
$env:VICTORY_FAST_COPY = '1'
# From fxceless/victory-fork main: copy-back through our own view, plain
# copies in our process, our threads on the fast cores (12.8 -> 13.8 fps).
$env:VICTORY_BACK_OWN_VIEW = '1'
$env:VICTORY_COPY_PLAIN = '1'
$env:VICTORY_FAST_CORES = '1'
# Measured on an RTX 4090 (24 GB), one run each, same scene: upstream's
# submit thread with joined parts (14.18 fps), then the title one frame
# ahead, longer joins, more title memory in the card's memory and fetches
# on the copy queue (14.96 fps, the card 64% busy instead of 80-90%).
$env:VICTORY_ASYNC_SUBMIT = '2'
$env:VICTORY_JOIN_PARTS = '1'
$env:VICTORY_ASYNC_PRESENT = '1'
$env:VICTORY_JOIN_MOST = '16'
$env:VICTORY_JOIN_LISTS = '256'
$env:VICTORY_QUEUE2 = '3'
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
# Windows in GPU_UPLOAD heaps (Resizable BAR: the CPU writes them straight
# into the card's memory, no staging side, no copy on the card) instead of
# card windows, and card copies for title buffers from 512 KiB up:
# opening cutscene 6.6-7.4 -> 8.4-8.9 fps with the view guard on, picture
# right (screenshots, 2026-11-25). Either one alone: ~7.3.
$env:VICTORY_CARD_WINDOWS_MB = '0'
$env:VICTORY_UPLOAD_WINDOWS_MB = '2048'
$env:VICTORY_CARD_BUFFERS_MIN_KB = '512'
if ($env:VICTORY_BIG_CARD -eq '1') { $env:VICTORY_UPLOAD_WINDOWS_MB = '6144'; $env:VICTORY_CARD_BUFFERS_MIN_KB = '64' }
# fxceless/victory-fork PR #868 (Chipsraine): what shaders write to card
# copies of write-combining title memory is not fetched back and compared.
# Copy-back 69-88 -> 10-14 ms, preparing 74-81 -> 45-55 ms, 8.0-8.9 fps
# against ~7.0 on the same build, picture right (screenshots, 2026-11-25).
# Risky only if the title's CPU reads that memory back.
$env:VICTORY_SKIP_WC_COMPARE = '1'
# The strides table and the address directory our translated shaders read
# in the card's memory (idea from ubiquitarianism/leonida's notes): RTX 5080,
# 10.3-11.0 fps against 9.7-10.0, picture right (2026-11-25 global test).
$env:VICTORY_TABLES_ON_CARD = '1'
# Intel CPUs: the title's monitorx/mwaitx waits sleep instead of trapping.
$env:VICTORY_MWAITX_SLEEP = '1'
# Room for textures: uploads left without card memory ~2,000 -> 20-50 a run.
$env:VICTORY_CARD_ROOM_FOR_TARGETS_MB = '1024'
$env:VICTORY_TEXTURE_ROOM_PERCENT = '94'
# Upstream's guard over the other views of a page stays ON: with
# VICTORY_VIEW_GUARD=0 (about +1 fps) walls of the opening cutscene's cell
# were missing (sky drawn in their place), with it on they are there
# (screenshots of the same shot, 2026-11-25).

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
    $exe = 'G:\GTA VI\victory-gta6\target\release\victory-lod15.exe'
}

& $exe run 'G:\GTA VI\GTA61.0.2.1\game_scarlett_master.exe' --execute --console series-x *> $log

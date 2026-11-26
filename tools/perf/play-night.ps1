# Launch GTA VI with the best set of the night of 2026-11-26 (branch integ/fps-15,
# docs/perf/nuit-2026-11-26.md): Thibault's clean set (dkdknight/victory-fork
# PR #9), the strides table and address directory on the card, ray node
# tests that answer "no hit" (no structure is built), the per-thread lookup
# cache, 24 pipeline workers. Picture checked right in the cell; ~10-10.6 fps
# on an RTX 5080.
#   powershell -ExecutionPolicy Bypass -File "G:\GTA VI\tools\play-night.ps1"
# Log: G:\GTA VI\profiles\night-run.log
# Own cache folder (victory-n4.toml): the first launch makes the pipelines
# (a few minutes slow at the start); later launches read them back.
param([string]$Exe = 'G:\GTA VI\victory-gta6\target\release\victory-n4.exe')

$out = 'G:\GTA VI\profiles'
New-Item -ItemType Directory -Force $out | Out-Null
Set-Location 'G:\GTA VI\victory-gta6'

$env:VICTORY_CONFIG = 'G:\GTA VI\victory-gta6\victory-n4.toml'
$env:VICTORY_DISPLAY_MODE = '1920x1080'
$env:VICTORY_RELAY = '2'
$env:VICTORY_LATE_WAIT = '3'
$env:VICTORY_WIDE_BACK = '1'
$env:VICTORY_CARD_BUFFERS_MIN_MB = '1'
$env:VICTORY_CARD_BUFFERS_MB = '1024'
$env:VICTORY_TABLES_MEMO = '2'
$env:VICTORY_FAST_COPY = '1'
$env:VICTORY_BACK_OWN_VIEW = '1'
$env:VICTORY_COPY_PLAIN = '1'
$env:VICTORY_FAST_CORES = '1'
$env:VICTORY_ASYNC_SUBMIT = '2'
$env:VICTORY_JOIN_PARTS = '1'
$env:VICTORY_JOIN_MOST = '16'
$env:VICTORY_JOIN_LISTS = '256'
$env:VICTORY_ASK_SHARED = '1'
$env:VICTORY_LIST_POOL_MOST = '1024'
$env:VICTORY_LIST_POOL_MB = '2048'
$env:VICTORY_INDIRECT_AMPLIFY = '1'
$env:VICTORY_AMPLIFY_WAIT = 'auto'
$env:VICTORY_CLAMP_WAIT = 'auto'
$env:VICTORY_CARD_REHASH_RUNS = '1'
$env:VICTORY_JOIN_COPIES = '1'
$env:VICTORY_LATE_FILL_AHEAD = '1'
$env:VICTORY_LATE_MERGE_RUNS = '1000000'
$env:VICTORY_LATE_MERGE_GAP = '64'
$env:VICTORY_CARD_WINDOWS_MB = '0'
$env:VICTORY_UPLOAD_WINDOWS_MB = '2048'
$env:VICTORY_CARD_BUFFERS_MIN_KB = '512'
$env:VICTORY_SKIP_WC_COMPARE = '1'
$env:VICTORY_MWAITX_SLEEP = '1'
$env:VICTORY_CARD_ROOM_FOR_TARGETS_MB = '1024'
$env:VICTORY_TEXTURE_ROOM_PERCENT = '94'
$env:VICTORY_LEVELS_ON_DEMAND = '1'
# The night's additions.
$env:VICTORY_TABLES_ON_CARD = '1'
$env:VICTORY_BVH_NO_HIT = '1'
$env:VICTORY_LOOKUP_CACHE = '1'
$env:VICTORY_PIPELINE_WORKERS = '24'

& $Exe run 'G:\GTA VI\GTA61.0.2.1\game_scarlett_master.exe' --execute --console series-x *> "$out\night-run.log"

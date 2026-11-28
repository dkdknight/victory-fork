# Launch the cool_fox base (G:\GTA VI\coolfox-base, branch base/coolfox) with our light settings.
# cool_fox's own launcher (Play-VIctory.cmd) settings, except:
# - the ray-traced passes kept (VICTORY_RT_RESOLVE=1, VICTORY_ACTIVATE_RAYS=1) and their node tests
#   answer "no hit" (VICTORY_BVH_NO_HIT=1, as on our integ/fps-15);
# - the title's own ambient light (no VICTORY_AMBIENT / VICTORY_AMBIENT_AIR stand-in);
# - saves shared with our builds (VICTORY_DATA_DIR).
# Series S profile, as cool_fox (-Console series-x for the other). Log: G:\GTA VI\profiles\coolfox-base-run.log
#
# In the game, F1 opens the settings menu (arrows pick and change, Enter does it; F3 the stats panel, F8 the live
# overlay). What it saves goes to -MenuFile (G:\GTA VI\victory-settings.txt) and is applied here at every start,
# over the settings below; an option given on the command line wins over the file. -NoMenuFile: the file is not
# read (A/B runs); -MenuWins: the file wins over the command line too (a restart from the menu). Delete the file, or FORGET SAVED SETTINGS in the menu, to go back to these settings.
param([string]$Exe = 'G:\GTA VI\target-coolfox-menu\release\victory-all.exe', [switch]$Warm, [string]$Resolve = '1', [string]$Activate = '1', [string]$Log = 'coolfox-base-run.log', [string]$LoopLimit = '', [string]$CacheName = 'cache-coolfox-fx', [string]$Fill = '1', [string]$Screen = 'DISPLAY7', [string]$Awake = '0', [string]$UpQueue = '0', [string]$DataDir = 'G:\GTA VI\saves-test\70pct\data', [ValidateSet('series-s', 'series-x')][string]$Console = 'series-s', [string]$MenuFile = 'G:\GTA VI\victory-settings.txt', [switch]$NoMenuFile, [switch]$MenuWins, [switch]$EnvOnly, [string]$Preload = '1', [switch]$PreloadAll)

$env:VICTORY_GAME_DIR = 'G:\GTA VI\GTA61.0.2.1'
$env:VICTORY_DATA_DIR = $DataDir   # -DataDir: another save folder (G:\GTA VI\saves-test\<name>\data)
$env:VICTORY_CACHE_DIR = "$env:LOCALAPPDATA\VIctory\$CacheName"
$env:VICTORY_TEXTURE_NO_WAIT = '0'   # 0 since 27/11 11:45: with 1, Jason's clothes were baked with black patches in the city (bakes found their textures missing); with 0, clean
$env:VICTORY_RELAY = '1'   # 1 since 27/11 15:15: stage 2 sent older words over the card's (up-copy check ~12,000 a minute, 0 with 1) = the black patches on Jason (user saw them only in stage-2 phases); costs ~1.5-2 fps in the city
$env:VICTORY_SECTION_MB = '2'   # Thibault's PR #14 (trustno1-coder/VIctory-NVIDIA#44, CLHelper): remaps 118 -> 14 ms/s on his 4090; neutral mean, fewer stalls
$env:VICTORY_WIDE_BACK = '1'
$env:VICTORY_CARD_REHASH_RUNS = '1'
$env:VICTORY_JOIN_COPIES = '1'
$env:VICTORY_INDIRECT_WINDOW = '1'
$env:VICTORY_RELAY_COPY = '1'
$env:VICTORY_CARD_STEPS = '2'
# Card steps at relay stage 1 too (VICTORY_RELAY=1 ran none: ~12 upload fills a frame waited for every flight, 50-58 ms):
# RTX 5080, Series X 1440p DLSS Quality, city save: 9.8 -> 15.6 fps, clean picture (trustno1-coder/VIctory-NVIDIA #295 lead).
$env:VICTORY_CARD_STEPS_RELAY1 = '1'
# The water's three wave64 programs run in the paired form on NVIDIA (else refused: dark navy water). fxceless
# 8a32d0ba/3ab5c12b/e3307c77/a9181638 (pointed at by corgi, Corgansm/victory-corgi) + our partial-sum fix.
if (-not $env:VICTORY_WAVE_PAIRS_ONLY) { $env:VICTORY_WAVE_PAIRS_ONLY = '5dc3b25986622949,6f225b641c5b0708,926c46bab3b86bd6' }
if (-not $env:VICTORY_WAVE_PAIRS_FP32) { $env:VICTORY_WAVE_PAIRS_FP32 = 'non-ieee-rne-ftz' }
# Low mips first (trustno1 R7, <= 32 ms a frame): a texture shows blurry at once instead of a stand-in (white foam
# on new water, black faces). The format-chosen stand-in is off: one format serves masks of opposite meaning.
if (-not $env:VICTORY_EARLY_TEXTURES_MS) { $env:VICTORY_EARLY_TEXTURES_MS = '32' }
if (-not $env:VICTORY_STANDIN_BY_FORMAT) { $env:VICTORY_STANDIN_BY_FORMAT = '0' }
# Best-of sessions 1, 2 and 4a (trustno1 CPU ports, El1xr, SantosFktt, Kemerd, cpu-2, trustno1 #57/#47, corgi C1),
# measured in-run (VICTORY_AB, all on against all off): RTX 4090 Series X city +11 %, +8 %, +7 %; RTX 5080 Series X
# +13 % and +9 % (two runs). FRESH_OPEN, WIDE_WAKE_ONE, BATCH_KEEP and HELPER_PRIORITY are on by default in the code.
foreach ($k in 'VICTORY_SHORT_REMAP_LOCK', 'VICTORY_TARGET_SYNC_CACHE', 'VICTORY_CARD_STEPS_FAST', 'VICTORY_SEND_FAST', 'VICTORY_WATCH_FAST', 'VICTORY_WIDE_BATCH', 'VICTORY_UNDER_PARTS', 'VICTORY_TITLE_WRITE_RANGE', 'VICTORY_REFRESH_PROBE_TEXTURES', 'VICTORY_SKIP_WC_BACK', 'VICTORY_SCRATCH_AT_LANDING', 'VICTORY_EXACT_SPARE', 'VICTORY_STORE_BESIDE', 'VICTORY_SOURCE_POOL', 'VICTORY_SKIP_DEAD_RT', 'VICTORY_DEPTH_TWIN_LATE',
    # session 5 (trustno1 #516 #501, Belobug #637 via joejumbo1): RTX 4090 +11 %, a 5080 in trustno1 #501 +10.8 %
    'VICTORY_FAULT_UNLOCKED', 'VICTORY_LANE_UNFOLD_FREE', 'VICTORY_LANE_WAKE_FREE') {
    if (-not (Test-Path "env:$k")) { Set-Item "env:$k" '1' }
}
if (-not $env:VICTORY_LAZY_COMPARE_MB) { $env:VICTORY_LAZY_COMPARE_MB = '64' }
# VICTORY_LAZY_NARROW left off: an access violation at the title image's +0x4e9b54a (read 0xc) on an RTX 5080,
# new game, 28/11 06:20 - the same address trustno1 #501 (POMXARK) reports with LAZY_NARROW.
# trustno1 #638 (every title view in a readable slot) is off: one more read per view access in every shader cost
# ~30 % here (RTX 5080 Series X city: 10.7 against 15.1 fps at ~10,000 draws); black textures have another cause.
if (-not $env:VICTORY_VIEW_REMAP) { $env:VICTORY_VIEW_REMAP = '0' }
$env:VICTORY_INDIRECT_BATCH = '2'
$env:VICTORY_MAP_THREADS = '1'
$env:VICTORY_CARD_AWAKE = $Awake   # 0 since 26/11 21:20: +2 fps with the up queue (19.5 -> 21.6 at the boat)
$env:VICTORY_UP_QUEUE = $UpQueue    # avatarmanz's copy queue: OFF since 27/11 02:00, in-run A/B gave 21.2 fps off vs 19.0 on
$env:VICTORY_QUICK_FIND_BIG = '1'    # corgi (and fxceless #1237): +~3 fps with FETCH_PARTS at the boat, 27/11 02:30 (18.3 -> 21.3 same phase)
$env:VICTORY_FETCH_PARTS = '1'       # corgi: the card's compare flags 4 KiB parts, only those come back
$env:VICTORY_MESH_LAUNCH = '1'       # fxceless #1381/0e006f0: in-run A/B +~0.5 fps, picture clean (27/11 04:00); changes translation (own cache)
$env:VICTORY_VRAM_HOMES = '1'        # jamazzz 958e1b7: GPU-only UAV buffers live on the card; with TEXTURE_HASH_FAST 20-21 -> 24-25 fps at the boat (27/11 04:10), copy-back 27 -> 7 MB
$env:VICTORY_TEXTURE_HASH_FAST = '1'  # fxceless #1395 (collision fixed here): faster texture content hash
$env:VICTORY_WIDE_COPY = '2'         # fxceless #1424 (+ title-memory copies): long copies read by several threads, in-run A/B +~0.8 fps, picture checked (27/11 04:45)
$env:VICTORY_SHADOW_SWEEP_LAZY = '1' # only the card copies touched since the last submission are swept: in-run A/B +~0.8 fps (5 rounds of 5), picture checked at the boat (27/11 07:15)
$env:VICTORY_COMPARE_WIDE = '1'       # our card compare, 4 threads a row instead of 1: in-run A/B in the city -4..5 ms a frame, fetches unchanged, picture checked (27/11 10:00)
$env:VICTORY_TEXTURE_POOL = '0'       # each texture its own allocation: in the city the 64 MB blocks held 4.2 GB free, VRAM 14.3 GB for a 12.9 budget (textures held back, black patches); with 0: 9.0 GB, picture clean (27/11 11:00)
$env:VICTORY_PANEL = '1'             # stats panel (F3 hides it)
$env:VICTORY_LIVE_FILE = '1'         # live settings: F8 in game, or coolfox-port6\scripts\live-menu.ps1
$env:VICTORY_NAN_GUARD = '1'          # fix/black-sun cd24cd8 (avatarmanz #329 / fxceless #837): the black sun is gone (user, 26/11 23:40)
$env:VICTORY_SUBMIT_FRAMES = '2'     # avatarmanz PR #379: the title records the next frame while ours is sent (title waits at presents 21 -> 0 ms)
$env:VICTORY_SHOW_AFTER = '1'
$env:VICTORY_THIN = '1'
$env:VICTORY_JOIN_PARTS = '8'
$env:VICTORY_UPLOAD_AHEAD = '0'     # 27/11 03:00 in-run A/B: on never helped, -1..2 fps in later phases
$env:VICTORY_LEVELS_ON_DEMAND = '1'
# VICTORY_CARD_BUFFERS_MIN_KB = '64' (cool_fox's) removed 27/11 12:45: black patches on Jason's clothes while he moved, in the city;
# with the default threshold (4 MB) none (seen by the user). Proudnoob4 saw black shards with 64 too (fxceless #1268).
$env:VICTORY_CARD_BUFFERS_MIN_KB = '64'   # back with RELAY=1 (the race it showed was the relay's stage 2)
# Pipelines (comparaison/preload-shaders.md): every pipeline the title creates is written down (the book,
# known-pipelines.bin beside the library), and before the title starts `victory run` makes the shaders and the
# pipelines it needs first while their stamps are stale (new translator, switches, driver or build), with a window
# (its buttons: play now, stop). The first time: a recording run of about a minute if no book yet, then 3-7 min;
# later starts < 5 s. -Preload 0: no barrier. -PreloadAll: also every known pipeline (~88,600) for the driver's
# cache, 20-40 min the first time.
if (-not $env:VICTORY_PIPELINE_BOOK) { $env:VICTORY_PIPELINE_BOOK = '1' }
$env:VICTORY_PRELOAD = $Preload
if ($PreloadAll) { $env:VICTORY_PRELOAD_ALL = '1' }
# Our light settings.
$env:VICTORY_RT_RESOLVE = $Resolve
$env:VICTORY_ACTIVATE_RAYS = $Activate
$env:VICTORY_BVH_NO_HIT = '1'
# Fills behind a copy at its place done on the card (fix/fill-on-card 6b9b733): the 186 ms
# settle per frame is gone, 4 -> 23 fps with both ray-traced passes kept. Override with -Fill 0.
$env:VICTORY_FILL_ON_CARD = $Fill
# The game window opens on this screen (feat/window-screen): DISPLAY7 is the top-right 4K
# screen. -Screen '' keeps the primary one.
$env:VICTORY_SCREEN = $Screen
# Diagnostics builds only: every translated loop cut at this many iterations.
if ($LoopLimit) { $env:VICTORY_SHADER_LOOP_LIMIT = $LoopLimit }
# Series X switches play-coolfox-x.ps1 sets, gone if it ran in this window (1440p refuses Series S).
foreach ($k in 'VICTORY_NATIVE_1440P', 'VICTORY_RENDER_1080P', 'VICTORY_DLSS', 'VICTORY_DLSS_MODE', 'VICTORY_FRAMEGEN', 'VICTORY_FRAMEGEN_X', 'VICTORY_TEXTURE_ROOM_PERCENT') {
    Remove-Item "Env:$k" -ErrorAction SilentlyContinue
}

# What the F1 menu saved (after trustno1-coder/VIctory-NVIDIA's play-foxhound.ps1): NAME=value lines, an empty
# value unsets the switch; VICTORY_MENU_CONSOLE is the console profile. An option given here explicitly keeps its value.
$explicit = @{
    Resolve = 'VICTORY_RT_RESOLVE'; Activate = 'VICTORY_ACTIVATE_RAYS'; Fill = 'VICTORY_FILL_ON_CARD'; Awake = 'VICTORY_CARD_AWAKE'
    UpQueue = 'VICTORY_UP_QUEUE'; Screen = 'VICTORY_SCREEN'; DataDir = 'VICTORY_DATA_DIR'; Console = 'VICTORY_MENU_CONSOLE'
}
$bound = $PSBoundParameters
$kept = if ($MenuWins) { @() } else { @($explicit.Keys | Where-Object { $bound.ContainsKey($_) } | ForEach-Object { $explicit[$_] }) }
if (-not $NoMenuFile -and (Test-Path -LiteralPath $MenuFile)) {
    $applied = @()
    foreach ($line in Get-Content -LiteralPath $MenuFile) {
        if ($line -notmatch '^\s*(VICTORY_[A-Z0-9_]+)\s*=(.*)$') { continue }
        $name = $Matches[1]; $value = $Matches[2].Trim()
        if ($name -in $kept) { continue }
        if ($name -eq 'VICTORY_MENU_CONSOLE') {
            if ($value -in 'series-s', 'series-x') { $Console = $value; $applied += "console=$value" }
            continue
        }
        if ($value) { Set-Item "Env:$name" $value } else { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
        $applied += "$name=$value"
    }
    if ($applied) { Write-Host "F1 menu settings ($MenuFile): $($applied -join ' ')" }
}
# Ray tracing off (F1 menu, VICTORY_RT=0): the title's ray-traced passes left out and an estimate of indirect
# light in their place, as trustno1-coder/VIctory-NVIDIA and readmet3xt-lab/foxhound run (Thibault's 4090, Series X
# city: 11.7 -> 16.9 fps). On (unset or 1): the title's own ray tracing, as before.
if ($env:VICTORY_RT -eq '0') {
    $env:VICTORY_RT_RESOLVE = '0'
    $env:VICTORY_ACTIVATE_RAYS = '0'
    if (-not $env:VICTORY_AMBIENT) { $env:VICTORY_AMBIENT = '0.8' }
    if (-not $env:VICTORY_AMBIENT_AIR) { $env:VICTORY_AMBIENT_AIR = '1.5' }
    if (-not $env:VICTORY_LEAVE_OUT) { $env:VICTORY_LEAVE_OUT = '02384a60fda6f2d0,731a39171b568298' }
}
# foxhound-sync switches (heaps' records and write-combined memory, ports of readmet3xt-lab/foxhound via
# trustno1-coder/VIctory-NVIDIA): measured on an RTX 4090 in Series X, 53.3 -> 49.3 ms a frame (partner
# measurement). Each one only when not already set (the F1 menu or this window wins). VICTORY_WC_NO_BACK
# overlaps cpu-2's VICTORY_SKIP_WC_BACK: keep only one of the two.
foreach ($k in 'VICTORY_HEAP_SYNC_FIRST', 'VICTORY_SYNC_IN_PLACE', 'VICTORY_SPANS_QUICK', 'VICTORY_SYNC_FINE', 'VICTORY_HEAP_SYNC_AT_SUBMIT', 'VICTORY_WC_NO_BACK') {
    if (-not (Test-Path "Env:$k")) { Set-Item "Env:$k" '1' }
}
if (-not $env:VICTORY_WC_NO_BACK_MIN_KB) { $env:VICTORY_WC_NO_BACK_MIN_KB = '1024' }
# Relay merge (fix/relay-stale): a dword both the card and the title changed stays the title's
# (VICTORY_RELAY_CONFLICT=title, implies the dword rule VICTORY_RELAY_MERGE_DWORD), and the kept dwords go back up
# to the card copy (VICTORY_RELAY_SENDBACK=1). Measured on an RTX 4090 in Series X by Thibault's agent: zero cost,
# 570-2,200 dwords a frame the card and the title both changed kept as the title's; torn words gave stretched
# meshes and black streaks on clothes. Each one only when not already set (the F1 menu or this window wins).
if (-not $env:VICTORY_RELAY_CONFLICT) { $env:VICTORY_RELAY_CONFLICT = 'title' }
if (-not $env:VICTORY_RELAY_SENDBACK) { $env:VICTORY_RELAY_SENDBACK = '1' }
# cpu-2 switches (VICTORY_SKIP_WC_BACK / _COMPARE / _KEEP, VICTORY_SCRATCH_AT_LANDING, VICTORY_EXACT_SPARE,
# VICTORY_PLACED_CENSUS) are left at their defaults here: still to be measured.
# Series X (from -Console or the F1 menu): native 1440p, and DLSS Quality with frame generation ready but off
# unless the menu saved otherwise (DLSS through NVIDIA Streamline, as play-coolfox-x.ps1; its files fetched by
# trustno1-coder/VIctory-NVIDIA's Get-NVIDIA-DLSS.cmd). DLSS off is refused at 1440p: the title's FSR cannot.
if ($Console -eq 'series-x') {
    if (-not $env:VICTORY_RENDER_1080P) { $env:VICTORY_NATIVE_1440P = '1' }
    if (-not $env:VICTORY_STREAMLINE_DIR) { $env:VICTORY_STREAMLINE_DIR = 'G:\GTA VI\trustno1\nvidia-sdk\Streamline\bin\x64' }
    if (Test-Path (Join-Path $env:VICTORY_STREAMLINE_DIR 'sl.interposer.dll')) {
        if (-not $env:VICTORY_DLSS) { $env:VICTORY_DLSS = '1' }
        if (-not $env:VICTORY_DLSS_MODE) { $env:VICTORY_DLSS_MODE = 'quality' }
        if (-not $env:VICTORY_FRAMEGEN) { $env:VICTORY_FRAMEGEN = '1' }
        if (-not $env:VICTORY_FRAMEGEN_X) { $env:VICTORY_FRAMEGEN_X = '0' }
        if (-not $env:VICTORY_TEXTURE_ROOM_PERCENT) { $env:VICTORY_TEXTURE_ROOM_PERCENT = if ($env:VICTORY_FRAMEGEN_X -ne '0') { '80' } else { '86' } }
    } else { Write-Host "No NVIDIA files in $($env:VICTORY_STREAMLINE_DIR): Series X without DLSS." }
}
# For the menu: where it saves, and how it starts the game again ("SAVE AND RESTART NOW": the same options, the
# saved settings winning over them).
$env:VICTORY_MENU_FILE = $MenuFile
$env:VICTORY_MENU_CONSOLE = $Console
$env:VICTORY_MENU_PLAY = $MyInvocation.MyCommand.Path
$env:VICTORY_MENU_ARGS = ((@($bound.GetEnumerator() | Where-Object { $_.Key -notin 'EnvOnly', 'Warm', 'MenuWins' } | ForEach-Object {
    if ($_.Value -is [System.Management.Automation.SwitchParameter]) { if ($_.Value.IsPresent) { "-$($_.Key)" } }
    else { "-$($_.Key) '$(([string]$_.Value) -replace "'", "''")'" }
})) + '-MenuWins') -join ' '

# -EnvOnly: only the settings (dot-source it, as tools\ngfx-trace-coolfox.ps1 does).
if ($EnvOnly) { return }
if ($Warm) {
    Set-Location 'G:\GTA VI\coolfox-base'
    & $Exe shaders
    return
}
# The game always runs as victory-play.exe, beside the build asked for, so that a per-program
# setting (the NVIDIA App's Smooth Motion, a GPU profile) keeps applying whatever the build.
# Copied again only when the build differs; if the old copy is in use, it runs as it is.
$Play = Join-Path (Split-Path $Exe) 'victory-play.exe'
if ($Exe -ne $Play) {
    $same = (Test-Path $Play) -and ((Get-FileHash $Exe).Hash -eq (Get-FileHash $Play).Hash)
    if (-not $same) {
        try {
            Copy-Item $Exe $Play -Force -ErrorAction Stop
            $pdb = [System.IO.Path]::ChangeExtension($Exe, '.pdb')
            if (Test-Path $pdb) { Copy-Item $pdb ([System.IO.Path]::ChangeExtension($Play, '.pdb')) -Force -ErrorAction SilentlyContinue }
        } catch { Write-Host "victory-play.exe is in use: the copy already there runs." }
    }
}
Set-Location 'G:\GTA VI\coolfox-base'
& $Play run "$env:VICTORY_GAME_DIR\game_scarlett_master.exe" --console $Console --execute *> "G:\GTA VI\profiles\$Log"

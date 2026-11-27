# Launch the cool_fox base (G:\GTA VI\coolfox-base, branch base/coolfox) with our light settings.
# cool_fox's own launcher (Play-VIctory.cmd) settings, except:
# - the ray-traced passes kept (VICTORY_RT_RESOLVE=1, VICTORY_ACTIVATE_RAYS=1) and their node tests
#   answer "no hit" (VICTORY_BVH_NO_HIT=1, as on our integ/fps-15);
# - the title's own ambient light (no VICTORY_AMBIENT / VICTORY_AMBIENT_AIR stand-in);
# - saves shared with our builds (VICTORY_DATA_DIR).
# Series S profile, as cool_fox. Log: G:\GTA VI\profiles\coolfox-base-run.log
param([string]$Exe = 'G:\GTA VI\target-coolfox-port7\release\victory-main2.exe', [switch]$Warm, [string]$Resolve = '1', [string]$Activate = '1', [string]$Log = 'coolfox-base-run.log', [string]$LoopLimit = '', [string]$CacheName = 'cache-coolfox-fx', [string]$Fill = '1', [string]$Screen = 'DISPLAY7', [string]$Awake = '0', [string]$UpQueue = '0', [string]$DataDir = 'G:\GTA VI\saves-test\70pct\data', [switch]$EnvOnly)

$env:VICTORY_GAME_DIR = 'G:\GTA VI\GTA61.0.2.1'
$env:VICTORY_DATA_DIR = $DataDir   # -DataDir: another save folder (G:\GTA VI\saves-test\<name>\data)
$env:VICTORY_CACHE_DIR = "$env:LOCALAPPDATA\VIctory\$CacheName"
$env:VICTORY_TEXTURE_NO_WAIT = '0'   # 0 since 27/11 11:45: with 1, Jason's clothes were baked with black patches in the city (bakes found their textures missing); with 0, clean
$env:VICTORY_RELAY = '1'   # 1 since 27/11 15:15: stage 2 sent older words over the card's (up-copy check ~12,000 a minute, 0 with 1) = the black patches on Jason (user saw them only in stage-2 phases); costs ~1.5-2 fps in the city
$env:VICTORY_WIDE_BACK = '1'
$env:VICTORY_CARD_REHASH_RUNS = '1'
$env:VICTORY_JOIN_COPIES = '1'
$env:VICTORY_INDIRECT_WINDOW = '1'
$env:VICTORY_RELAY_COPY = '1'
$env:VICTORY_CARD_STEPS = '2'
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
& $Play run "$env:VICTORY_GAME_DIR\game_scarlett_master.exe" --console series-s --execute *> "G:\GTA VI\profiles\$Log"

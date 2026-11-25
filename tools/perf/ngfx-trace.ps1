# Launch GTA VI (branch feature/levels-on-demand, build victory-lod15.exe, play-lod.ps1's settings)
# under Nsight Graphics GPU Trace. Press F11 on the game window once the opening cutscene is on
# screen: 3 frames are traced and the metrics exported to G:\GTA VI\profiles\ngfx.
#   powershell -ExecutionPolicy Bypass -File "G:\GTA VI\tools\ngfx-trace.ps1"
$N = 'C:\Program Files\NVIDIA Corporation\Nsight Graphics 2026.4.0\host\windows-desktop-nomad-x64'
$out = 'G:\GTA VI\profiles\ngfx'
New-Item -ItemType Directory -Force $out | Out-Null
$vars = [ordered]@{
  NO_COLOR = '1'; VICTORY_CONFIG = 'G:\GTA VI\victory-gta6\victory-lod.toml'; VICTORY_DISPLAY_MODE = '1920x1080'
  VICTORY_RELAY = '2'; VICTORY_LATE_WAIT = '3'; VICTORY_WIDE_BACK = '1'; VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'
  VICTORY_TABLES_MEMO = '2'; VICTORY_ASYNC_SUBMIT = '1'; VICTORY_FAST_COPY = '1'; VICTORY_ASK_SHARED = '1'
  VICTORY_LIST_POOL_MOST = '1024'; VICTORY_LIST_POOL_MB = '2048'; VICTORY_INDIRECT_AMPLIFY = '1'; VICTORY_AMPLIFY_WAIT = 'auto'
  VICTORY_CLAMP_WAIT = 'auto'; VICTORY_CARD_REHASH_RUNS = '1'; VICTORY_JOIN_COPIES = '1'; VICTORY_LATE_FILL_AHEAD = '1'
  VICTORY_LATE_MERGE_RUNS = '1000000'; VICTORY_LATE_MERGE_GAP = '64'; VICTORY_CARD_ROOM_FOR_TARGETS_MB = '1024'
  VICTORY_TEXTURE_ROOM_PERCENT = '94'; VICTORY_CARD_WINDOWS_MB = '0'; VICTORY_UPLOAD_WINDOWS_MB = '2048'
  VICTORY_CARD_BUFFERS_MIN_KB = '512'; VICTORY_SKIP_WC_COMPARE = '1'; VICTORY_LEVELS_ON_DEMAND = '1'
}
$envArg = (($vars.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join '; ') + ';'
$argList = @(
  # (No --platform: Qt takes "--platform <name>" as its own platform plugin and fails to start.)
  '--activity', '"GPU Trace Profiler"', '--no-timeout',
  '--exe', '"G:\GTA VI\victory-gta6\target\release\victory-lod15.exe"', '--dir', '"G:\GTA VI\victory-gta6"',
  '--args', '"run \"G:\GTA VI\GTA61.0.2.1\game_scarlett_master.exe\" --execute --console series-x"',
  '--env', ('"' + $envArg + '"'), '--output-dir', ('"' + $out + '"'),
  '--start-after-hotkey', '--limit-to-frames', '3', '--max-duration-ms', '3000',
  '--architecture', '"Blackwell GB20x"', '--metric-set-id', '0', '--auto-export'
)
Set-Location $N
Write-Host "GTA VI under Nsight GPU Trace: press A at the start screen if needed, then F11 during the opening cutscene."
$p = Start-Process -FilePath "$N\ngfx.exe" -ArgumentList $argList -NoNewWindow -Wait -PassThru `
  -RedirectStandardOutput "$out\ngfx-run.txt" -RedirectStandardError "$out\ngfx-err.txt"
Write-Host "ngfx finished (exit $($p.ExitCode)). Output: $out"

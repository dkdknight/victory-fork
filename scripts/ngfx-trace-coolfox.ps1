# The cool_fox build (play-coolfox.ps1's settings and save) under Nsight Graphics GPU Trace:
# 3 frames traced -StartMs after launch (the city of the 70 percent save is on screen by
# ~240 s), metrics exported to G:\GTA VI\profiles\ngfx-city.
param([int]$StartMs = 300000, [string]$Keys = '', [string]$Exe = 'G:\GTA VI\target-coolfox-port7\release\victory-lazy.exe')
. 'G:\GTA VI\tools\play-coolfox.ps1' -EnvOnly
if ($Keys) { $env:VICTORY_KEYS = $Keys }
$N = 'C:\Program Files\NVIDIA Corporation\Nsight Graphics 2026.4.0\host\windows-desktop-nomad-x64'
$out = 'G:\GTA VI\profiles\ngfx-city'
New-Item -ItemType Directory -Force $out | Out-Null
$envArg = ((Get-ChildItem env: | Where-Object { $_.Name -like 'VICTORY_*' } | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join '; ') + ';'
$game = "$env:VICTORY_GAME_DIR\game_scarlett_master.exe"
# (The call operator quotes each argument itself; Start-Process with hand-made quotes
# never launched the game.)
& "$N\ngfx.exe" --activity 'GPU Trace Profiler' --no-timeout `
  --exe $Exe --dir 'G:\GTA VI\coolfox-base' `
  --args ('run \"' + $game + '\" --console series-s --execute') `
  --env $envArg --output-dir $out `
  --start-after-ms $StartMs --limit-to-frames 3 --max-duration-ms 3000 `
  --architecture 'Blackwell GB20x' --metric-set-id 0 --auto-export
"ngfx exited $LASTEXITCODE"
Get-ChildItem $out -Recurse -Filter *.ngfx-gputrace | Sort-Object LastWriteTime | Select-Object -Last 1 FullName, Length

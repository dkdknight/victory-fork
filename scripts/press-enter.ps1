# Presses Enter in the game window every few seconds (for unattended runs of builds where VICTORY_KEYS
# does not work in release). Brings the window to the front before each press.
param([int]$Every = 8, [int]$For = 240, [string]$Name = 'victory')
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class K {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
}
'@
$end = (Get-Date).AddSeconds($For)
while ((Get-Date) -lt $end) {
    $p = Get-Process -Name $Name -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    if ($p) {
        [void][K]::ShowWindow($p.MainWindowHandle, 9)
        [void][K]::SetForegroundWindow($p.MainWindowHandle)
        Start-Sleep -Milliseconds 300
        [K]::keybd_event(0x0D, 0x1C, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 120
        [K]::keybd_event(0x0D, 0x1C, 2, [UIntPtr]::Zero)
    }
    Start-Sleep -Seconds $Every
}

# VIctory live settings: a window of sliders, outside the game, always on top.
# Ported from Corgansm/victory-corgi (scripts/live-menu.ps1).
#
# It changes the settings of a running victory.exe through the file named by
# VICTORY_LIVE_FILE (crates\victory-gpu\src\live.rs): victory.exe writes the
# file with the run's values and <file>.menu with the settings when a run
# starts, and reads the file again whenever it changes; a change is taken at
# the title's next present. The overlay in the game (F8) changes the same
# file; this window follows it. VICTORY_LIVE_FILE=1 is the file beside
# VICTORY_DATA_DIR (G:\GTA VI\victory-gta6-data\live-settings.txt with
# VICTORY_DATA_DIR=G:\GTA VI\victory-gta6-data\data), the default here.
#
#   powershell -STA -ExecutionPolicy Bypass -File live-menu.ps1 [-Settings <file>]
param([string]$Settings = 'G:\GTA VI\victory-gta6-data\live-settings.txt')
$File = $Settings

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

# Started with a hidden console (-WindowStyle Hidden), Windows hides the first window this process shows too: the
# form is shown again by hand once it is up.
Add-Type -Namespace VIctory -Name Win -MemberDefinition '[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int show);'
$script:shown = $false

$menuPath = "$File.menu"
$script:rows = [ordered]@{}
$script:loading = $false
$script:seenFile = [datetime]::MinValue
$script:seenMenu = [datetime]::MinValue

$back = [System.Drawing.Color]::FromArgb(16, 20, 24)
$fore = [System.Drawing.Color]::FromArgb(230, 230, 230)
$font = New-Object System.Drawing.Font('Consolas', 10)

$form = New-Object System.Windows.Forms.Form
$form.Text = 'VIctory live settings'
$form.TopMost = $true
$form.FormBorderStyle = 'FixedToolWindow'
$form.BackColor = $back
$form.ForeColor = $fore
$form.Font = $font
$form.ClientSize = New-Object System.Drawing.Size(560, 120)
$form.StartPosition = 'Manual'
$area = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
$form.Location = New-Object System.Drawing.Point(($area.Right - 580), ($area.Top + 40))

$status = New-Object System.Windows.Forms.Label
$status.AutoSize = $false
$status.Location = New-Object System.Drawing.Point(10, 8)
$status.Size = New-Object System.Drawing.Size(540, 40)
$status.ForeColor = [System.Drawing.Color]::FromArgb(180, 180, 180)
$status.Text = "waiting for VIctory: start a run with VICTORY_LIVE_FILE=$File (or =1)"
$form.Controls.Add($status)

function Format-Value($spec, [double]$v) {
    if ($spec.Switch) { if ($v -ge 0.5) { return 'on' } else { return 'off' } }
    if ($spec.Step -ge 1) { return $v.ToString('0', [Globalization.CultureInfo]::InvariantCulture) }
    if ($spec.Step -ge 0.1) { return $v.ToString('0.0', [Globalization.CultureInfo]::InvariantCulture) }
    return $v.ToString('0.00', [Globalization.CultureInfo]::InvariantCulture)
}

function Get-RowValue($row) {
    if ($row.Spec.Switch) { if ($row.Check.Checked) { return 1.0 } else { return 0.0 } }
    return [math]::Round($row.Spec.Least + $row.Bar.Value * $row.Spec.Step, 3)
}

function Set-RowValue($row, [double]$v) {
    if ($row.Spec.Switch) { $row.Check.Checked = ($v -ge 0.5); return }
    $ticks = [int][math]::Round(($v - $row.Spec.Least) / $row.Spec.Step)
    $row.Bar.Value = [math]::Max($row.Bar.Minimum, [math]::Min($row.Bar.Maximum, $ticks))
    $row.Shown.Text = Format-Value $row.Spec (Get-RowValue $row)
}

function Write-Settings {
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $lines = @('# VIctory live settings: NAME=value (VICTORY_LIVE_FILE)')
    foreach ($name in $script:rows.Keys) {
        $lines += ('{0}={1}' -f $name, (Get-RowValue $script:rows[$name]).ToString($inv))
    }
    $text = ($lines -join "`n") + "`n"
    $part = "$File.part"
    try {
        [IO.File]::WriteAllText($part, $text)
        Move-Item -LiteralPath $part -Destination $File -Force
    } catch {
        try { [IO.File]::WriteAllText($File, $text) } catch { $status.Text = "not written: $($_.Exception.Message)" }
    }
    try { $script:seenFile = (Get-Item -LiteralPath $File).LastWriteTimeUtc } catch {}
}

function Read-Settings {
    $values = @{}
    try {
        foreach ($line in [IO.File]::ReadAllLines($File)) {
            $at = $line.IndexOf('=')
            if ($line.StartsWith('#') -or $at -lt 1) { continue }
            $v = 0.0
            if ([double]::TryParse($line.Substring($at + 1).Trim(), [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$v)) {
                $values[$line.Substring(0, $at).Trim()] = $v
            }
        }
    } catch {}
    return $values
}

function Build-Rows {
    foreach ($row in $script:rows.Values) { foreach ($c in $row.Controls) { $form.Controls.Remove($c); $c.Dispose() } }
    $script:rows = [ordered]@{}
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $y = 56
    foreach ($line in [IO.File]::ReadAllLines($menuPath)) {
        $f = $line.Split('|')
        if ($f.Count -lt 6) { continue }
        $spec = [pscustomobject]@{
            Name = $f[0]; Label = $f[1]
            Least = [double]::Parse($f[2], $inv); Most = [double]::Parse($f[3], $inv)
            Step = [double]::Parse($f[4], $inv); Switch = ($f[5] -eq '1')
        }
        $label = New-Object System.Windows.Forms.Label
        $label.Text = $spec.Label
        $label.AutoSize = $false
        $label.Location = New-Object System.Drawing.Point(10, ($y + 6))
        $label.Size = New-Object System.Drawing.Size(220, 22)
        $form.Controls.Add($label)
        $row = [pscustomobject]@{ Spec = $spec; Bar = $null; Check = $null; Shown = $null; Controls = @($label) }
        if ($spec.Switch) {
            $check = New-Object System.Windows.Forms.CheckBox
            $check.Location = New-Object System.Drawing.Point(240, ($y + 4))
            $check.Size = New-Object System.Drawing.Size(200, 24)
            $check.Text = 'on'
            $check.Tag = $spec.Name
            $check.Add_CheckedChanged({ param($sender, $e) if (-not $script:loading) { Write-Settings } })
            $form.Controls.Add($check)
            $row.Check = $check
            $row.Controls += $check
        } else {
            $bar = New-Object System.Windows.Forms.TrackBar
            $bar.Location = New-Object System.Drawing.Point(235, $y)
            $bar.Size = New-Object System.Drawing.Size(240, 30)
            $bar.Minimum = 0
            $bar.Maximum = [int][math]::Round(($spec.Most - $spec.Least) / $spec.Step)
            $bar.TickStyle = 'None'
            $bar.SmallChange = 1
            $bar.LargeChange = [math]::Max(1, [int]($bar.Maximum / 10))
            $bar.Tag = $spec.Name
            $shown = New-Object System.Windows.Forms.Label
            $shown.AutoSize = $false
            $shown.Location = New-Object System.Drawing.Point(482, ($y + 6))
            $shown.Size = New-Object System.Drawing.Size(70, 22)
            $bar.Add_ValueChanged({
                param($sender, $e)
                $r = $script:rows[$sender.Tag]
                $r.Shown.Text = Format-Value $r.Spec (Get-RowValue $r)
                if (-not $script:loading) { Write-Settings }
            })
            $form.Controls.Add($bar)
            $form.Controls.Add($shown)
            $row.Bar = $bar
            $row.Shown = $shown
            $row.Controls += @($bar, $shown)
        }
        $script:rows[$spec.Name] = $row
        $y += 34
    }
    $form.ClientSize = New-Object System.Drawing.Size(560, ($y + 10))
}

function Load-Values {
    $values = Read-Settings
    $script:loading = $true
    try {
        foreach ($name in $script:rows.Keys) {
            if ($values.ContainsKey($name)) { Set-RowValue $script:rows[$name] $values[$name] }
        }
    } finally { $script:loading = $false }
}

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 300
$timer.Add_Tick({
    if (-not $script:shown) {
        $script:shown = $true
        [void][VIctory.Win]::ShowWindow($form.Handle, 5)
        $form.TopMost = $true
        $form.BringToFront()
    }
    try {
        if (-not (Test-Path -LiteralPath $menuPath)) { return }
        $m = (Get-Item -LiteralPath $menuPath).LastWriteTimeUtc
        if ($m -ne $script:seenMenu) {
            $script:seenMenu = $m
            Build-Rows
            $script:seenFile = [datetime]::MinValue
            $status.Text = "connected: $File`nchanges are taken at the next frame"
        }
        if (Test-Path -LiteralPath $File) {
            $t = (Get-Item -LiteralPath $File).LastWriteTimeUtc
            $dragging = [System.Windows.Forms.Control]::MouseButtons -ne 'None' -and $form.ContainsFocus
            if ($t -ne $script:seenFile -and -not $dragging) {
                $script:seenFile = $t
                Load-Values
            }
        }
    } catch {
        $status.Text = "error: $($_.Exception.Message)"
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)

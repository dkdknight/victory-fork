# VIctory live settings: a window of sliders, outside the game, always on top, labels in French.
# Ported from Corgansm/victory-corgi (scripts/live-menu.ps1, its "VIctory live settings" window), with the
# settings of this tree.
#
# It changes the settings of a running victory.exe through the file named by VICTORY_LIVE_FILE
# (crates\victory-gpu\src\live.rs): victory.exe writes the file with the run's values and <file>.menu with the
# settings when a run starts (name|label|least|most|step|switch|French label, one a line), and reads the file
# again whenever it changes; a change is taken at the title's next present. <file>.render holds the title's
# render and output sizes (render_w render_h output_w output_h dlss). The overlay in the game (F8) and the F1
# menu change the same file; this window follows them. VICTORY_LIVE_FILE=1 is the file beside VICTORY_DATA_DIR,
# in its parent folder (G:\GTA VI\saves-test\70pct\live-settings.txt with play-coolfox.ps1's default -DataDir).
#
# victory-corgi's window has: render scale (next start), ambient light, crease shade, crease radius, air light
# limit, card buffers cap, BAR windows cap, stats panel, DXR reader, ray tracing mode, frame rate cap, and a
# restart button. Here first the ones this tree has (VICTORY_AMBIENT, VICTORY_AMBIENT_SHADE_STRENGTH,
# VICTORY_AMBIENT_SHADE_RADIUS, VICTORY_AMBIENT_AIR, VICTORY_CARD_BUFFERS_MB, VICTORY_WINDOWS_BAR_MB,
# VICTORY_PANEL, VICTORY_FRAME_CAP), then this tree's other live settings. Left out: the render scale (read once
# by the loader; DLSS picks the scene's size in Series X), the DXR reader and the ray tracing modes (not in this
# tree).
#
# "Restart game (plain start)" closes the game (-Process) and starts the launcher again (-Launcher, with
# -LaunchArgs less -LiveWindow), with none of this window's VICTORY_* variables: the launcher's own settings
# and what the F1 menu saved. This window stays and follows the new run.
#
#   powershell -STA -ExecutionPolicy Bypass -File live-menu.ps1 [-Settings <file>] [-Launcher <ps1>] [-LaunchArgs <args>] [-Process victory-play]
# play-coolfox.ps1 -LiveWindow opens it with all of these. One window at a time.
param(
    [string]$Settings = 'G:\GTA VI\saves-test\70pct\live-settings.txt',
    [string]$Launcher = $env:VICTORY_MENU_PLAY,
    [string]$LaunchArgs = $env:VICTORY_MENU_ARGS,
    [string]$Process = 'victory-play'
)
$File = $Settings

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

# One window: a second one (the launcher started again with -LiveWindow) leaves at once.
$created = $false
$script:single = New-Object System.Threading.Mutex($true, 'Local\VIctoryLiveSettingsWindow', [ref]$created)
if (-not $created) { return }
# The game comes first.
try { [System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'BelowNormal' } catch {}

# Started with a hidden console (-WindowStyle Hidden), Windows hides the first window this process shows too: the
# form is shown again by hand once it is up.
Add-Type -Namespace VIctory -Name Win -MemberDefinition '[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int show);'
$script:shown = $false

$menuPath = "$File.menu"
$renderPath = "$File.render"
$script:rows = [ordered]@{}
$script:extra = @()
$script:loading = $false
$script:seenFile = [datetime]::MinValue
$script:seenMenu = [datetime]::MinValue
$script:seenRender = [datetime]::MinValue
$script:renderText = 'taille de rendu : pas encore connue'
$script:gameText = ''
$script:ticks = 0
$inv = [Globalization.CultureInfo]::InvariantCulture
$fr = [Globalization.CultureInfo]::GetCultureInfo('fr-FR')

# victory-corgi's window's settings that this tree has, in their order; the others after them.
$corgi = @('VICTORY_AMBIENT', 'VICTORY_AMBIENT_SHADE_STRENGTH', 'VICTORY_AMBIENT_SHADE_RADIUS', 'VICTORY_AMBIENT_AIR',
    'VICTORY_CARD_BUFFERS_MB', 'VICTORY_WINDOWS_BAR_MB', 'VICTORY_PANEL', 'VICTORY_FRAME_CAP')

$back = [System.Drawing.Color]::FromArgb(16, 20, 24)
$fore = [System.Drawing.Color]::FromArgb(230, 230, 230)
$dim = [System.Drawing.Color]::FromArgb(160, 160, 160)
$font = New-Object System.Drawing.Font('Consolas', 10)
$width = 660

$form = New-Object System.Windows.Forms.Form
$form.Text = 'VIctory : réglages en direct (d''après corgi)'
$form.TopMost = $true
$form.FormBorderStyle = 'FixedToolWindow'
$form.BackColor = $back
$form.ForeColor = $fore
$form.Font = $font
$form.ClientSize = New-Object System.Drawing.Size($width, 150)
$form.StartPosition = 'Manual'
$area = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
$form.Location = New-Object System.Drawing.Point(($area.Right - $width - 20), ($area.Top + 40))
$tips = New-Object System.Windows.Forms.ToolTip

$status = New-Object System.Windows.Forms.Label
$status.AutoSize = $false
$status.Location = New-Object System.Drawing.Point(10, 8)
$status.Size = New-Object System.Drawing.Size(($width - 20), 58)
$status.ForeColor = [System.Drawing.Color]::FromArgb(180, 180, 180)
$form.Controls.Add($status)

function Show-Status([string]$first) {
    if ($first) { $script:head = $first }
    $status.Text = "$($script:head)`n$($script:renderText)`n$($script:gameText)"
}
$script:head = "en attente de VIctory : lancer une partie avec VICTORY_LIVE_FILE=$File (ou =1)"
Show-Status ''

$restart = New-Object System.Windows.Forms.Button
$restart.Text = 'Redémarrer le jeu (lancement simple)'
$restart.FlatStyle = 'Flat'
$restart.ForeColor = $fore
$restart.Size = New-Object System.Drawing.Size(340, 30)
$restart.Location = New-Object System.Drawing.Point(10, 100)
$restart.Enabled = [bool]$Launcher -and (Test-Path -LiteralPath $Launcher)
$tips.SetToolTip($restart, "Ferme $Process.exe et relance $Launcher avec ses options et les réglages enregistrés par F1.")
$restart.Add_Click({
    $answer = [System.Windows.Forms.MessageBox]::Show($form, 'Fermer le jeu et le relancer maintenant ? La partie non sauvegardée est perdue.', 'VIctory', 'YesNo', 'Question')
    if ($answer -ne 'Yes') { return }
    Get-Process -Name $Process -ErrorAction SilentlyContinue | Stop-Process -Force
    for ($i = 0; $i -lt 40 -and (Get-Process -Name $Process -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 250 }
    # A plain start: the launcher's options without -LiveWindow (this window stays), and none of the VICTORY_*
    # variables this window got from the launcher that started it.
    $options = ([string]$LaunchArgs -replace '(^|\s)-LiveWindow(?=\s|$)', ' ').Trim()
    Get-ChildItem Env: | Where-Object { $_.Name -like 'VICTORY_*' } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
    $command = "& '$($Launcher -replace "'", "''")' $options"
    Start-Process powershell -WindowStyle Minimized -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', "`"$command`""
    Show-Status 'redémarrage du jeu ...'
})
$form.Controls.Add($restart)

$note = New-Object System.Windows.Forms.Label
$note.AutoSize = $false
$note.Size = New-Object System.Drawing.Size(($width - 20), 20)
$note.ForeColor = $dim
$note.Text = "Pris en compte à l'image suivante. F8 en jeu : les mêmes réglages."
$form.Controls.Add($note)

function Format-Value($spec, [double]$v) {
    if ($spec.Switch) { if ($v -ge 0.5) { return 'oui' } else { return 'non' } }
    if ($spec.Step -ge 1) { return $v.ToString('0', $fr) }
    if ($spec.Step -ge 0.1) { return $v.ToString('0.0', $fr) }
    return $v.ToString('0.00', $fr)
}

function Get-RowValue($row) {
    if ($row.Spec.Switch) { if ($row.Check.Checked) { return 1.0 } else { return 0.0 } }
    return [math]::Round($row.Spec.Least + $row.Bar.Value * $row.Spec.Step, 3)
}

function Set-RowValue($row, [double]$v) {
    if ($row.Spec.Switch) { $row.Check.Checked = ($v -ge 0.5); $row.Check.Text = Format-Value $row.Spec $v; return }
    $ticks = [int][math]::Round(($v - $row.Spec.Least) / $row.Spec.Step)
    $row.Bar.Value = [math]::Max($row.Bar.Minimum, [math]::Min($row.Bar.Maximum, $ticks))
    $row.Shown.Text = Format-Value $row.Spec (Get-RowValue $row)
}

function Write-Settings {
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
        try { [IO.File]::WriteAllText($File, $text) } catch { Show-Status "non écrit : $($_.Exception.Message)" }
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
            if ([double]::TryParse($line.Substring($at + 1).Trim(), [Globalization.NumberStyles]::Float, $inv, [ref]$v)) {
                $values[$line.Substring(0, $at).Trim()] = $v
            }
        }
    } catch {}
    return $values
}

function Add-Heading([string]$text, [int]$y) {
    $h = New-Object System.Windows.Forms.Label
    $h.Text = $text
    $h.AutoSize = $false
    $h.ForeColor = [System.Drawing.Color]::FromArgb(100, 255, 120)
    $h.Location = New-Object System.Drawing.Point(10, ($y + 4))
    $h.Size = New-Object System.Drawing.Size(($width - 20), 20)
    $form.Controls.Add($h)
    $script:extra += $h
}

function Add-Row($spec, [int]$y) {
    $label = New-Object System.Windows.Forms.Label
    $label.Text = $spec.Label
    $label.AutoSize = $false
    $label.Location = New-Object System.Drawing.Point(10, ($y + 6))
    $label.Size = New-Object System.Drawing.Size(300, 22)
    $tips.SetToolTip($label, $spec.Name)
    $form.Controls.Add($label)
    $row = [pscustomobject]@{ Spec = $spec; Bar = $null; Check = $null; Shown = $null; Controls = @($label) }
    if ($spec.Switch) {
        $check = New-Object System.Windows.Forms.CheckBox
        $check.Location = New-Object System.Drawing.Point(320, ($y + 4))
        $check.Size = New-Object System.Drawing.Size(200, 24)
        $check.Text = 'non'
        $check.Tag = $spec.Name
        $check.Add_CheckedChanged({
            param($sender, $e)
            $sender.Text = if ($sender.Checked) { 'oui' } else { 'non' }
            if (-not $script:loading) { Write-Settings }
        })
        $form.Controls.Add($check)
        $row.Check = $check
        $row.Controls += $check
    } else {
        $bar = New-Object System.Windows.Forms.TrackBar
        $bar.Location = New-Object System.Drawing.Point(315, $y)
        $bar.Size = New-Object System.Drawing.Size(250, 30)
        $bar.Minimum = 0
        $bar.Maximum = [int][math]::Round(($spec.Most - $spec.Least) / $spec.Step)
        $bar.TickStyle = 'None'
        $bar.SmallChange = 1
        $bar.LargeChange = [math]::Max(1, [int]($bar.Maximum / 10))
        $bar.Tag = $spec.Name
        $shown = New-Object System.Windows.Forms.Label
        $shown.AutoSize = $false
        $shown.Location = New-Object System.Drawing.Point(572, ($y + 6))
        $shown.Size = New-Object System.Drawing.Size(80, 22)
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
}

function Build-Rows {
    foreach ($row in $script:rows.Values) { foreach ($c in $row.Controls) { $form.Controls.Remove($c); $c.Dispose() } }
    foreach ($c in $script:extra) { $form.Controls.Remove($c); $c.Dispose() }
    $script:rows = [ordered]@{}
    $script:extra = @()
    $specs = @()
    foreach ($line in [IO.File]::ReadAllLines($menuPath)) {
        $f = $line.Split('|')
        if ($f.Count -lt 6) { continue }
        # The French label (seventh field) when the build writes it.
        $label = if ($f.Count -ge 7 -and $f[6].Trim()) { $f[6].Trim() } else { $f[1] }
        $specs += [pscustomobject]@{
            Name = $f[0]; Label = $label
            Least = [double]::Parse($f[2], $inv); Most = [double]::Parse($f[3], $inv)
            Step = [double]::Parse($f[4], $inv); Switch = ($f[5] -eq '1')
        }
    }
    $first = @($corgi | ForEach-Object { $n = $_; $specs | Where-Object { $_.Name -eq $n } })
    $others = @($specs | Where-Object { $_.Name -notin $corgi })
    $y = 70
    if ($first.Count) {
        Add-Heading "Réglages de corgi (Corgansm/victory-corgi)" $y
        $y += 26
        foreach ($spec in $first) { Add-Row $spec $y; $y += 34 }
    }
    if ($others.Count) {
        Add-Heading 'Nos autres réglages en direct' $y
        $y += 26
        foreach ($spec in $others) { Add-Row $spec $y; $y += 34 }
    }
    $note.Location = New-Object System.Drawing.Point(10, ($y + 4))
    $restart.Location = New-Object System.Drawing.Point(10, ($y + 28))
    $height = $y + 68
    # Taller than the screen: the window scrolls.
    $most = $area.Height - 80
    $form.AutoScroll = $height -gt $most
    $form.ClientSize = New-Object System.Drawing.Size(($width + $(if ($form.AutoScroll) { 18 } else { 0 })), ([math]::Min($height, $most)))
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
        # Whether the game runs, every 1.5 s.
        if ($script:ticks++ % 5 -eq 0) {
            $running = [bool](Get-Process -Name $Process -ErrorAction SilentlyContinue)
            $text = if ($running) { "jeu : en marche ($Process.exe)" } else { "jeu : arrêté ($Process.exe introuvable)" }
            if ($text -ne $script:gameText) { $script:gameText = $text; Show-Status '' }
        }
        if (-not (Test-Path -LiteralPath $menuPath)) { return }
        $m = (Get-Item -LiteralPath $menuPath).LastWriteTimeUtc
        if ($m -ne $script:seenMenu) {
            $script:seenMenu = $m
            Build-Rows
            $script:seenFile = [datetime]::MinValue
            Show-Status "fichier connecté : $File"
        }
        if (Test-Path -LiteralPath $File) {
            $t = (Get-Item -LiteralPath $File).LastWriteTimeUtc
            $dragging = [System.Windows.Forms.Control]::MouseButtons -ne 'None' -and $form.ContainsFocus
            if ($t -ne $script:seenFile -and -not $dragging) {
                $script:seenFile = $t
                Load-Values
            }
        }
        if (Test-Path -LiteralPath $renderPath) {
            $r = (Get-Item -LiteralPath $renderPath).LastWriteTimeUtc
            if ($r -ne $script:seenRender) {
                $script:seenRender = $r
                $s = ([IO.File]::ReadAllText($renderPath).Trim() -split '\s+')
                if ($s.Count -ge 4) {
                    $by = if ($s.Count -ge 5 -and $s[4] -eq '1') { 'DLSS' } else { 'FidelityFX du jeu' }
                    $script:renderText = "rendu actuel $($s[0])x$($s[1]) -> sortie $($s[2])x$($s[3]) ($by)"
                    Show-Status ''
                }
            }
        } elseif ($script:seenRender -ne [datetime]::MinValue) {
            $script:seenRender = [datetime]::MinValue
            $script:renderText = 'taille de rendu : pas encore connue'
            Show-Status ''
        }
    } catch {
        Show-Status "erreur : $($_.Exception.Message)"
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)

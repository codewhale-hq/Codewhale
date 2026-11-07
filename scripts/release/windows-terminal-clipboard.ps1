param(
    [Parameter(Mandatory)][string]$Terminal,
    [Parameter(Mandatory)][string]$Binary,
    [Parameter(Mandatory)][string]$Workspace,
    [Parameter(Mandatory)][string]$Title,
    [Parameter(Mandatory)][string]$Evidence
)
$ErrorActionPreference = 'Stop'
[IO.File]::WriteAllText([IO.Path]::Combine($Evidence, 'desktop-started.txt'), "startedAt=$([DateTime]::UtcNow.ToString('o'))`npid=$PID`npowershellVersion=$($PSVersionTable.PSVersion)`n")
Write-Output 'desktop stage: loading automation'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ClipboardFixtureWindow {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int command);
}
'@
$window = $null
$ownedPid = $null
Write-Output 'desktop stage: automation loaded'
function Read-Terminal {
    $texts = $window.FindAll([System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty, $true))
    return (@($texts | ForEach-Object {
        $_.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
    }) -join "`n")
}
function Wait-For([scriptblock]$Check, [string]$Label) {
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path (Join-Path $Evidence 'cancel')) { throw 'Clipboard check canceled' }
        if (& $Check) { return }
        Start-Sleep -Milliseconds 200
    }
    throw "Deadline waiting for $Label"
}
function Capture([string]$Name) {
    (Read-Terminal) | Set-Content -Encoding UTF8 (Join-Path $Evidence "$Name.txt")
    $r = $window.Current.BoundingRectangle
    if ($r.IsEmpty -or $r.Width -le 0 -or $r.Height -le 0) { throw 'Terminal has no visible window' }
    $bitmap = [System.Drawing.Bitmap]::new([int]$r.Width, [int]$r.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen([int]$r.X, [int]$r.Y, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $Evidence "$Name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
}
function Focus-OwnedWindow {
    $process = Get-Process -Id $ownedPid
    if ($process.Path -ne $Terminal) { throw 'Terminal process identity changed' }
    $handle = [IntPtr]$window.Current.NativeWindowHandle
    [void][ClipboardFixtureWindow]::ShowWindow($handle, 9)
    [void][ClipboardFixtureWindow]::SetForegroundWindow($handle)
    Wait-For { [ClipboardFixtureWindow]::GetForegroundWindow() -eq $handle } 'owned Terminal focus'
}
try {
    # Reject quotes in the owned fixture arguments.
    foreach ($arg in @($Terminal, $Binary, $Workspace, $Title)) {
        if ($arg.Contains('"')) { throw 'Unexpected quote in fixture argument' }
    }
    $arguments = "-w new new-tab --title `"$Title`" --suppressApplicationTitle --startingDirectory `"$Workspace`" `"$Binary`" --workspace `"$Workspace`" --no-project-config --skip-onboarding --fresh"
    Write-Output 'desktop stage: starting owned Terminal'
    Start-Process -FilePath $Terminal -ArgumentList $arguments | Out-Null
    Write-Output 'desktop stage: locating owned Terminal window'
    Wait-For {
        $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
        foreach ($candidate in $windows) {
            if ($candidate.Current.Name.Contains($Title)) {
                $process = Get-Process -Id $candidate.Current.ProcessId
                if ($process.Path -eq $Terminal) {
                    $script:window = $candidate; $script:ownedPid = $process.Id; return $true
                }
            }
        }
        return $false
    } 'owned Windows Terminal window'
    Write-Output 'desktop stage: focusing owned Terminal'
    Focus-OwnedWindow
    Write-Output 'desktop stage: reading composer accessibility'
    Wait-For { (Read-Terminal).Contains('Type a message') } 'Codewhale composer'
    Write-Output 'desktop stage: composer ready'
    Capture 'ready'
    @{ processId = $ownedPid; readyAt = [DateTime]::UtcNow.ToString('o') } | ConvertTo-Json |
        Set-Content -Encoding UTF8 (Join-Path $Evidence 'ready.json')
    $payload = [System.IO.File]::ReadAllText((Join-Path $Evidence 'expected.txt'), [System.Text.Encoding]::UTF8)
    [System.Windows.Forms.Clipboard]::SetText($payload)
    if ([System.Windows.Forms.Clipboard]::GetText() -ne $payload) { throw 'Clipboard readback mismatch' }
    Focus-OwnedWindow
    [System.Windows.Forms.SendKeys]::SendWait('+{INS}')
    # Let every pasted line arrive before stashing the exact composer text.
    Start-Sleep -Milliseconds 1800
    Capture 'pasted'
    @{ processId = $ownedPid; sessionId = (Get-Process -Id $ownedPid).SessionId; chars = $payload.Length; input = 'Windows clipboard + Shift+Insert' } |
        ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $Evidence 'pasted.json')
    Wait-For { Test-Path (Join-Path $Evidence 'stash') } 'draft stash observation'
    Focus-OwnedWindow
    [System.Windows.Forms.SendKeys]::SendWait('^g')
    Wait-For { (Read-Terminal).Contains('Draft stashed') } 'actual composer stash'
    Capture 'completed'
    @{ stashKey = 'Ctrl+G'; harnessSubmitKeys = 0; completedAt = [DateTime]::UtcNow.ToString('o') } | ConvertTo-Json |
        Set-Content -Encoding UTF8 (Join-Path $Evidence 'completed.json')
} catch {
    if ($window) { try { Capture 'failure' } catch { Write-Warning $_ } }
    throw
} finally {
    # Retire only processes at this fixture's unique portable Terminal path.
    $stopped = @()
    foreach ($process in @(Get-Process | Where-Object { $_.Path -eq $Terminal })) {
        Stop-Process -InputObject $process -Force
        if (!$process.WaitForExit(5000)) { throw 'Owned Terminal did not exit' }
        $stopped += $process.Id
    }
    $remaining = @(Get-Process | Where-Object { $_.Path -eq $Terminal }).Count
    if ($remaining) { throw 'Owned Terminal is still running' }
    @{ terminal = $Terminal; stopped = $stopped; remaining = $remaining } | ConvertTo-Json |
        Set-Content -Encoding UTF8 (Join-Path $Evidence 'cleanup.json')
}

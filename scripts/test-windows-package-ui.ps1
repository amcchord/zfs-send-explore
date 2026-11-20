# Run from an unlocked interactive Windows desktop using Windows PowerShell -STA.
# Fixture payload hashes come from the original source bytes in tests/cli.rs
# and tests/desktop_service.rs. The test launches only the GUI inside the ZIP.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Package,
    [Parameter(Mandatory)][string]$FixtureDirectory,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$ExpectedPackageSha256,
    [int]$MinimumDpi = 120,
    [int]$MaximumDpi = 192
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Package = (Resolve-Path -LiteralPath $Package).Path
$FixtureDirectory = (Resolve-Path -LiteralPath $FixtureDirectory).Path
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Choose a new output directory for this run.' }
$OutputDirectory = (New-Item -ItemType Directory -Path $OutputDirectory).FullName
$Report = [ordered]@{ package = (Get-FileHash -LiteralPath $Package).Hash; success = $false }
$History = Join-Path $OutputDirectory 'steps.txt'
function Step([string]$Message) {
    $Message | Add-Content -LiteralPath $History
    Write-Host $Message
}
function Wait-For([string]$Description, [scriptblock]$Check) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $result = & $Check
        if ($result) { return $result }
        if ($Client.HasExited) { throw "Application closed while waiting for $Description" }
        Start-Sleep -Milliseconds 200
    } while ($timer.Elapsed.TotalSeconds -lt 25)
    throw "Timed out waiting for $Description"
}

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class PackageUI {
    public delegate bool EnumCallback(IntPtr w, IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumCallback callback, IntPtr data);
    [DllImport("user32.dll")] static extern int GetWindowThreadProcessId(IntPtr w, out int pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr w);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr w, StringBuilder b, int len);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr w, StringBuilder b, int len);
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr w, int id);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr w, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr w, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,EntryPoint="SendMessageW")] public static extern IntPtr SetText(IntPtr w, uint msg, IntPtr wp, string text);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,EntryPoint="SendMessageW")] static extern IntPtr ReadText(IntPtr w, uint msg, IntPtr wp, StringBuilder text);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr w);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr w);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr w, out Rect r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr w, IntPtr dc, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    public static IntPtr Window(int pid, string kind, string titlePrefix) {
        IntPtr result = IntPtr.Zero;
        EnumWindows(delegate(IntPtr w, IntPtr data) {
            int owner;
            GetWindowThreadProcessId(w, out owner);
            if (pid != owner || !IsWindowVisible(w)) return true;
            var cls = new StringBuilder(256);
            var title = new StringBuilder(512);
            GetClassName(w, cls, cls.Capacity);
            GetWindowText(w, title, title.Capacity);
            if (cls.ToString() != kind || !title.ToString().StartsWith(titlePrefix, StringComparison.OrdinalIgnoreCase)) return true;
            result = w;
            return false;
        }, IntPtr.Zero);
        return result;
    }
    public static string ComboItem(IntPtr combo, int index) {
        int length = (int)SendMessage(combo, 0x149, (IntPtr)index, IntPtr.Zero);
        if (length < 0 || length > 8192) throw new Exception("Bad restore point label");
        var label = new StringBuilder(length + 1);
        ReadText(combo, 0x148, (IntPtr)index, label);
        return label.ToString();
    }
}
'@

function Visible([string]$Kind, [string]$Title = '') {
    $handle = [PackageUI]::Window($Client.Id, $Kind, $Title)
    if ($handle -ne [IntPtr]::Zero) { $handle }
}
function Save-Window([IntPtr]$Window, [string]$Name, [switch]$Desktop) {
    $size = New-Object PackageUI+Rect
    if (![PackageUI]::GetWindowRect($Window, [ref]$size)) { throw 'Cannot measure the application window' }
    $bitmap = New-Object Drawing.Bitmap(($size.Right - $size.Left), ($size.Bottom - $size.Top))
    try {
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            if ($Desktop) {
                # PrintWindow can paint stale combo-box text over the current selection.
                # Capture the visible desktop pixels after the normal application repaint.
                Wait-For 'the application foreground window' { [PackageUI]::GetForegroundWindow() -eq $Window } | Out-Null
                Start-Sleep -Milliseconds 400
                $graphics.CopyFromScreen($size.Left, $size.Top, 0, 0, $bitmap.Size)
            } else {
                $dc = $graphics.GetHdc()
                try {
                    if (![PackageUI]::PrintWindow($Window, $dc, 2)) { throw 'Cannot capture the application window' }
                } finally { $graphics.ReleaseHdc($dc) }
            }
        } finally { $graphics.Dispose() }
        $bitmap.Save((Join-Path $OutputDirectory "$Name.png"), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $bitmap.Dispose() }
}
function Click-Button([IntPtr]$Parent, [int]$Id) {
    $button = [PackageUI]::GetDlgItem($Parent, $Id)
    if ($button -eq [IntPtr]::Zero -or ![PackageUI]::PostMessage($button, 0xf5, [IntPtr]::Zero, [IntPtr]::Zero)) {
        throw "Could not press button $Id"
    }
}
function Element([IntPtr]$Window, $Property, $Value) {
    $root = [Windows.Automation.AutomationElement]::FromHandle($Window)
    $match = New-Object Windows.Automation.PropertyCondition($Property, $Value)
    $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $match)
}
function Labels([IntPtr]$Window) {
    $root = [Windows.Automation.AutomationElement]::FromHandle($Window)
    $nodes = $root.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)
    ($nodes | ForEach-Object { $_.Current.Name }) -join ' '
}
function Enter-Filename([IntPtr]$Dialog, [string]$Path) {
    [PackageUI]::SetForegroundWindow($Dialog) | Out-Null
    Wait-For 'the file dialog keyboard focus' { [PackageUI]::GetForegroundWindow() -eq $Dialog } | Out-Null
    [System.Windows.Forms.SendKeys]::SendWait('%n') # File name mnemonic
    $edit = Wait-For 'the common dialog file name field' {
        $focused = [Windows.Automation.AutomationElement]::FocusedElement
        if ($focused.Current.ProcessId -eq $Client.Id -and $focused.Current.ControlType -eq [Windows.Automation.ControlType]::Edit) { $focused }
    }
    $edit.SetFocus()
    [System.Windows.Forms.SendKeys]::SendWait('^a')
    $keystrokes = -join ($Path.ToCharArray() | ForEach-Object {
        if ('+^%~(){}[]'.Contains($_)) { '{' + $_ + '}' } else { $_ }
    })
    [System.Windows.Forms.SendKeys]::SendWait($keystrokes)
    $value = $edit.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
    Wait-For 'the requested full file name to appear' { $value.Current.Value -eq $Path } | Out-Null
    if ([PackageUI]::GetForegroundWindow() -ne $Dialog) { throw 'The file dialog lost keyboard focus' }
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
}
function Open-Backup([string]$Name, [string]$Screenshot) {
    Step "Open $Name through the system file dialog"
    Click-Button $Main 101
    $dialog = Wait-For 'Open a backup' { Visible '#32770' 'Open a backup' }
    Save-Window $dialog $Screenshot
    Enter-Filename $dialog (Join-Path $FixtureDirectory $Name)
}
function List-Item([string]$Name) {
    $list = [PackageUI]::GetDlgItem($Main, 109)
    Element $list ([Windows.Automation.AutomationElement]::NameProperty) $Name
}
function Select-File([string]$Name) {
    $item = Wait-For "the file $Name" { List-Item $Name }
    $item.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
}
function Restore-To([string]$Path) {
    Click-Button $Main 110
    $dialog = Wait-For 'the restore save dialog' { Visible '#32770' 'Restore file' }
    Enter-Filename $dialog $Path
}
function Restore-And-Check([string]$Name, [string]$Path, [string]$Expected, [string]$Screenshot) {
    Step "Restore $Name and independently hash the saved file"
    Select-File $Name
    Restore-To $Path
    $done = Wait-For 'the restore confirmation' {
        $dialog = Visible '#32770' 'ZFS Explore'
        if ($dialog -and (Labels $dialog) -match 'Restored') { $dialog }
    }
    Save-Window $done $Screenshot
    $actual = (Get-FileHash -LiteralPath $Path).Hash
    if ($actual -ne $Expected) { throw "Wrong SHA-256 for $Name. Expected $Expected, received $actual" }
    $Report[$Name] = @{ expected = $Expected; restored = $actual }
    $close = Element $done ([Windows.Automation.AutomationElement]::NameProperty) 'Close'
    if (!$close) { throw 'Restore confirmation lacked the Close button' }
    $close.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-For 'restore confirmation to close' { !(Visible '#32770' 'ZFS Explore') } | Out-Null
}
function Enter-FixtureKey([IntPtr]$Dialog, [string]$Value) {
    $edit = [PackageUI]::GetDlgItem($Dialog, 200)
    if ($edit -eq [IntPtr]::Zero) { throw 'Missing masked credential field' }
    [PackageUI]::SetText($edit, 0xc, [IntPtr]::Zero, $Value) | Out-Null
    Click-Button $Dialog 1
}

$DpiContext = [PackageUI]::SetThreadDpiAwarenessContext([IntPtr](-4))
try {
    if ($Report.package -ne $ExpectedPackageSha256) { throw 'Package digest differs from the supplied archive hash' }
    $sources = @('encrypted-raw-s1.zfs', 'multi-snapshot.zfs')
    $sourceHashes = @($sources | ForEach-Object { (Get-FileHash -LiteralPath (Join-Path $FixtureDirectory $_)).Hash })
    $Report.fixtures = [ordered]@{}
    for ($i = 0; $i -lt $sources.Count; $i++) { $Report.fixtures[$sources[$i]] = $sourceHashes[$i] }
    $unpacked = Join-Path $OutputDirectory 'unpacked'
    Expand-Archive -LiteralPath $Package -DestinationPath $unpacked
    $exe = @(Get-ChildItem -LiteralPath $unpacked -Recurse -File -Filter 'zfs-send-explore-windows.exe')
    if ($exe.Count -ne 1) { throw 'Expected one packaged Windows GUI executable' }
    $Report.executable = (Get-FileHash -LiteralPath $exe[0].FullName).Hash
    $Client = Start-Process -FilePath $exe[0].FullName -PassThru
    $Main = Wait-For 'the main application window' { Visible 'ZfsSendExploreWindows' }
    $Report.dpi = [PackageUI]::GetDpiForWindow($Main)
    Step "Application window DPI is $($Report.dpi)"
    Save-Window $Main '01-welcome'
    if ($Report.dpi -lt $MinimumDpi -or $Report.dpi -gt $MaximumDpi) { throw 'Display scale outside required range' }

    Open-Backup 'encrypted-raw-s1.zfs' '02-open-backup'
    $unlock = Wait-For 'the unlock dialog' { Visible 'ZfsExploreFormDialog' }
    Step 'Enter a wrong fixture key and require an explicit retry'
    Enter-FixtureKey $unlock 'wrong-fixture-key'
    $retry = Wait-For 'the wrong-key retry message' {
        $form = Visible 'ZfsExploreFormDialog'
        if ($form -and (Labels $form) -like "*key didn't unlock*") { $form }
    }
    Save-Window $retry '03-wrong-key'
    Enter-FixtureKey $retry 'zfs-send-fixture-passphrase'
    $folder = Wait-For 'the unlocked docs folder' { List-Item 'docs' }
    $folder.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
    [PackageUI]::PostMessage($Main, 0x111, [IntPtr]133, [IntPtr]::Zero) | Out-Null # Open selected
    Restore-And-Check 'hello.txt' (Join-Path $OutputDirectory 'hello.txt') '6e219bc096755477b1a30301446ac364f6291f16ce381d561947cdecd3fab265' '04-decrypted-copy'

    Open-Backup 'multi-snapshot.zfs' '05-open-history'
    $combo = [PackageUI]::GetDlgItem($Main, 104)
    Wait-For 'three restore points' { [PackageUI]::SendMessage($combo, 0x146, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32() -eq 3 } | Out-Null
    $labels = @(0..2 | ForEach-Object { [PackageUI]::ComboItem($combo, $_) })
    $Report.restorePoints = $labels
    $current = [PackageUI]::SendMessage($combo, 0x147, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32()
    if ($current -lt 0 -or $labels[$current] -notmatch '\bs3\b') { throw 'Newest restore point s3 was not selected by default' }
    $older = @(0..2 | Where-Object { $labels[$_] -match '\bs1\b' })
    if ($older.Count -ne 1) { throw 'Older snapshot s1 must be identifiable in the list' }
    Step 'Select older s1 from the restore point list'
    [PackageUI]::SendMessage($combo, 0x14e, [IntPtr]$older[0], [IntPtr]::Zero) | Out-Null
    [PackageUI]::PostMessage($Main, 0x111, [IntPtr](0x10000 + 104), $combo) | Out-Null
    Wait-For 'the older version.txt' { List-Item 'version.txt' } | Out-Null
    Save-Window $Main '06-older-snapshot' -Desktop
    Restore-And-Check 'version.txt' (Join-Path $OutputDirectory 'version.txt') '490f9cafcac339449f3c241818c4798a7515c5f39832d213e1d79cac88f2ebf2' '07-older-copy'

    Step 'Attempt another restore to an occupied name and check the sentinel'
    $occupied = Join-Path $OutputDirectory 'occupied.txt'
    [IO.File]::WriteAllText($occupied, [guid]::NewGuid().ToString())
    $before = (Get-FileHash -LiteralPath $occupied).Hash
    Select-File 'version.txt'
    Restore-To $occupied
    $refusal = Wait-For 'same-name refusal' {
        $dialog = Visible '#32770' 'ZFS Explore'
        if ($dialog -and (Labels $dialog) -match 'name is already there|already exists') { $dialog }
    }
    Save-Window $refusal '08-same-name-refused'
    $after = (Get-FileHash -LiteralPath $occupied).Hash
    $Report.occupied = @{ before = $before; after = $after }
    if ($before -ne $after) { throw 'The restore changed the preexisting test file' }
    foreach ($source in $sources) {
        if ((Get-FileHash -LiteralPath (Join-Path $FixtureDirectory $source)).Hash -ne $Report.fixtures[$source]) {
            throw "The source fixture changed: $source"
        }
    }
    $Report.success = $true
    Step 'Passed'
} catch {
    $Report.error = $_.Exception.Message
    Step "FAILED $($Report.error) at line $($_.InvocationInfo.ScriptLineNumber)"
    if ($Main) { Save-Window $Main 'failure-main' }
    if ($Client) {
        foreach ($kind in '#32770','ZfsExploreFormDialog') {
            $window = Visible $kind
            if ($window) { Save-Window $window "failure-dialog-$($kind.TrimStart('#'))" }
        }
    }
    throw
} finally {
    $Report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'result.json') -Encoding UTF8
    if ($Client -and !$Client.HasExited) { $Client.Kill(); $Client.WaitForExit() }
    if ($DpiContext -ne [IntPtr]::Zero) { [PackageUI]::SetThreadDpiAwarenessContext($DpiContext) | Out-Null }
}

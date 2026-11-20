# Dot-sourced by test-windows-package-ui.ps1 after launching the packaged client.
# Browse mode expects the two read-only fixture disks to be attached by the caller.
# Restore mode takes unpacked fixed-VHD test copies left.vhd and right.vhd,
# pool-name, late.sha256 and vhd.sha256. An optional replacement.vhd should have
# the same model and size but a different device ID. Use a disposable desktop.
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class PickerInput {
    [StructLayout(LayoutKind.Sequential)]
    struct ThreadInfo {
        public uint size, flags;
        public IntPtr active, focus, capture, menu, move, caret;
        public int left, top, right, bottom;
    }
    [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint thread, ref ThreadInfo info);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll", EntryPoint="SendMessageW", CharSet=CharSet.Unicode)]
    static extern IntPtr ReadText(IntPtr window, uint message, IntPtr length, StringBuilder text);
    public static string Text(IntPtr window) {
        var text = new StringBuilder(1024);
        ReadText(window, 0xd, (IntPtr)text.Capacity, text);
        return text.ToString();
    }
    public static IntPtr Focus() {
        var info = new ThreadInfo();
        info.size = (uint)Marshal.SizeOf(info);
        return GetGUIThreadInfo(0, ref info) ? info.focus : IntPtr.Zero;
    }
}
'@

function Send-PickerKeys([IntPtr]$Window, [string]$Keys) {
    if ([PackageUI]::GetForegroundWindow() -ne $Window) { throw 'Keyboard target lost focus' }
    [Windows.Forms.SendKeys]::SendWait($Keys)
}
function Tab-To([IntPtr]$Window, [IntPtr]$Control) {
    for ($i = 0; $i -lt 12; $i++) {
        if ([PickerInput]::Focus() -eq $Control) { return }
        Send-PickerKeys $Window '{TAB}'
        Start-Sleep -Milliseconds 40
    }
    throw 'Could not reach the requested control with Tab'
}
function Picker-Count([IntPtr]$List) {
    [PackageUI]::SendMessage($List, 0x1004, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32()
}
function Row-Checked([IntPtr]$List, [int]$Index) {
    $state = [PackageUI]::SendMessage($List, 0x102c, [IntPtr]$Index, [IntPtr]0xf000).ToInt32()
    ($state -band 0xf000) -eq 0x2000
}
function Open-Picker {
    [PackageUI]::SetForegroundWindow($Main) | Out-Null
    Send-PickerKeys $Main '^d'
    Wait-For 'the connected drive checklist' { Visible '#32770' 'Connected drives' }
}
function Dismiss-Picker([IntPtr]$Window) {
    Send-PickerKeys $Window '{ESC}'
    Wait-For 'the previous application window' { [PackageUI]::GetForegroundWindow() -eq $Main } | Out-Null
}
function Attach-PickerImage([string]$Path) {
    $before = @(Get-Disk | ForEach-Object Number)
    if ((Get-DiskImage -ImagePath $Path).Attached) { throw "Fixture already attached: $Path" }
    $image = Mount-DiskImage -ImagePath $Path -Access ReadOnly -NoDriveLetter -PassThru
    $attached.Add($Path)
    $disk = $image | Get-Disk
    if (!$disk.IsReadOnly -or $disk.IsBoot -or $disk.IsSystem -or $disk.Number -in $before) {
        throw "Unexpected device mapping for $Path"
    }
    [int]$disk.Number
}
function Detach-PickerImage([string]$Path) {
    if (!$attached.Contains($Path)) { throw 'Requested disk was not attached by this run' }
    $image = Get-DiskImage -ImagePath $Path
    if ($image.ImagePath -ne $Path -or !$image.Attached) { throw 'Fixture mapping changed' }
    $disk = $image | Get-Disk
    if (!$disk.IsReadOnly -or $disk.IsBoot -or $disk.IsSystem) { throw 'Unexpected disk before detach' }
    Dismount-DiskImage -ImagePath $Path | Out-Null
    $attached.Remove($Path) | Out-Null
}
function Test-PickerRestore {
    # The generated fixture's root has documents, across.bin and guest.img.
    $dialog = Wait-For 'dataset selection' { Visible 'ZfsExploreFormDialog' }
    $combo = [PackageUI]::GetDlgItem($dialog, 200)
    $dataset = (Get-Content -Raw -LiteralPath (Join-Path $FixtureDirectory 'pool-name')).Trim() + '/files'
    $count = [PackageUI]::SendMessage($combo, 0x146, [IntPtr]::Zero, [IntPtr]::Zero).ToInt32()
    $rows = @(for ($i = 0; $i -lt $count; $i++) { if ([PackageUI]::ComboItem($combo,$i) -eq $dataset) { $i } })
    if ($rows.Count -ne 1) { throw 'Did not open the expected pool' }
    [PackageUI]::SendMessage($combo, 0x14e, [IntPtr]$rows[0], [IntPtr]::Zero) | Out-Null
    Click-Button $dialog 1
    $files = [PackageUI]::GetDlgItem($Main, 109)
    Wait-For 'all fixture files to be visible' { (Picker-Count $files) -eq 3 } | Out-Null
    # Reopening and cancelling must preserve the working pool.
    $cancel = Open-Picker
    if ([PickerInput]::IsWindowEnabled([PackageUI]::GetDlgItem($cancel, 1))) { throw 'Reopened picker retained a previous selection' }
    Dismiss-Picker $cancel
    Step 'Cancelled the reopened picker and returned to the working pool'
    Tab-To $Main $files
    Send-PickerKeys $Main '{HOME}{DOWN}'
    Wait-For 'the 32 MiB file selection' { [PackageUI]::SendMessage($files,0x100c,[IntPtr](-1),[IntPtr]2).ToInt32() -eq 1 } | Out-Null
    Send-PickerKeys $Main '^s'
    $save = Wait-For 'the Windows Save dialog' { Visible '#32770' 'Restore file' }
    $target = Join-Path $OutputDirectory 'across.bin'
    Send-PickerKeys $save '%n'
    Send-PickerKeys $save '^a'
    $escaped = -join ($target.ToCharArray() | ForEach-Object { if ('+^%~(){}[]'.Contains($_)) { '{' + $_ + '}' } else { $_ } })
    Send-PickerKeys $save $escaped
    Send-PickerKeys $save '{ENTER}'
    $done = Wait-For 'restore completion' {
        $result = Visible '#32770' 'ZFS Explore'
        if ($result -and (Labels $result) -match 'Restored') { $result }
    }
    Save-Window $done '08-restored'
    $expected = ((Get-Content -LiteralPath (Join-Path $FixtureDirectory 'late.sha256') | Select-String '  across\.bin$').Line -split '\s+')[0]
    $actual = (Get-FileHash -LiteralPath $target).Hash
    if (!$expected -or $actual -ne $expected) { throw 'Restored file failed the independent OpenZFS SHA-256 check' }
    $Report.restored = @{ source = $expected; restored = $actual; bytes = (Get-Item -LiteralPath $target).Length }
}

function Test-ConnectedDrivePicker {
    $attached = New-Object 'System.Collections.Generic.List[string]'
    $picker = Open-Picker
    $list = [PackageUI]::GetDlgItem($picker,2101)
    $showSystem = [PackageUI]::GetDlgItem($picker,2102)
    $refresh = [PackageUI]::GetDlgItem($picker,2103)
    $open = [PackageUI]::GetDlgItem($picker,1)
    if ($DrivePickerTest -eq 'Browse') {
        Wait-For 'two fixture disks visible to a standard user' { (Picker-Count $list) -eq 2 } | Out-Null
        if ((Row-Checked $list 0) -or (Row-Checked $list 1) -or [PickerInput]::IsWindowEnabled($open)) {
            throw 'A drive was preselected'
        }
        Tab-To $picker $list
        Send-PickerKeys $picker '{HOME} {DOWN} '
        Wait-For 'both fixture drives checked' { (Row-Checked $list 0) -and (Row-Checked $list 1) } | Out-Null
        Save-Window $picker '02-standard-fixture-disks' -Desktop
        Dismiss-Picker $picker
        Step 'Discovered two connected backup disks and cancelled after selecting them'
        return
    }
    Wait-For 'the boot disk to be hidden on the clean guest' { (Picker-Count $list) -eq 0 } | Out-Null
    if ([PickerInput]::IsWindowEnabled($open)) { throw 'Open was enabled with no selections' }
    Save-Window $picker '02-empty' -Desktop
    Tab-To $picker $showSystem
    Send-PickerKeys $picker ' '
    Wait-For 'one visible unselected Windows system disk' { (Picker-Count $list) -eq 1 -and !(Row-Checked $list 0) } | Out-Null
    if ([PickerInput]::IsWindowEnabled($open)) { throw 'Revealing the system disk selected it' }
    Save-Window $picker '03-system-revealed' -Desktop
    Send-PickerKeys $picker ' '
    Wait-For 'the system disk to be hidden again' { (Picker-Count $list) -eq 0 } | Out-Null
    Dismiss-Picker $picker
    Step 'The empty picker hid the boot disk, required a choice, and cancelled using Escape'
    if ($DrivePickerTest -eq 'Empty') { return }

    if (@(Get-Disk | Where-Object { !$_.IsBoot -and !$_.IsSystem }).Count) {
        throw 'Restore mode requires a disposable guest with no other data disks attached'
    }
    $images = @('left.vhd', 'right.vhd')
    if (Test-Path -LiteralPath (Join-Path $FixtureDirectory 'replacement.vhd')) { $images += 'replacement.vhd' }
    $imageHashes = @{}
    foreach ($name in $images) {
        $path = (Resolve-Path -LiteralPath (Join-Path $FixtureDirectory $name)).Path
        $expected = ((Get-Content -LiteralPath (Join-Path $FixtureDirectory 'vhd.sha256') | Where-Object { $_ -like "* $name" }) -split '\s+')[0]
        $imageHashes[$path] = (Get-FileHash -LiteralPath $path).Hash
        if ($imageHashes[$path] -ne $expected) { throw "Unexpected VHD contents for $name" }
    }
    $left = Join-Path $FixtureDirectory 'left.vhd'
    $right = Join-Path $FixtureDirectory 'right.vhd'
    $picker = Open-Picker
    $list = [PackageUI]::GetDlgItem($picker,2101)
    $refresh = [PackageUI]::GetDlgItem($picker,2103)
    $open = [PackageUI]::GetDlgItem($picker,1)
    try {
        Step 'Attach the first read-only VHD while the empty picker is open'
        $leftId = Attach-PickerImage $left
        Wait-For 'the first connected drive to appear automatically' { (Picker-Count $list) -eq 1 } | Out-Null
        Step 'Attach the second read-only member without closing the picker'
        $rightId = Attach-PickerImage $right
        Wait-For 'the second connected drive to appear automatically' { (Picker-Count $list) -eq 2 } | Out-Null
        if ($leftId -ge $rightId) { throw 'Expected the fixture disks in increasing numeric order' }
        Tab-To $picker $list
        Send-PickerKeys $picker '{HOME} '
        Wait-For 'first member selected' { (Row-Checked $list 0) -and !(Row-Checked $list 1) } | Out-Null
        Wait-For 'Open enabled for one drive' { [PickerInput]::IsWindowEnabled($open) } | Out-Null
        Send-PickerKeys $picker ' '
        Wait-For 'Open disabled again after clearing the last checkmark' { ![PickerInput]::IsWindowEnabled($open) } | Out-Null
        Send-PickerKeys $picker ' '
        Send-PickerKeys $picker '{DOWN} '
        Wait-For 'both members selected' { (Row-Checked $list 0) -and (Row-Checked $list 1) } | Out-Null
        Wait-For 'Open enabled for both members' { [PickerInput]::IsWindowEnabled($open) } | Out-Null
        Save-Window $picker '04-two-selected' -Desktop
        Step 'Refresh the checklist with two members selected by keyboard'
        Tab-To $picker $refresh
        Send-PickerKeys $picker ' '
        Wait-For 'unchanged selected identities after refresh' { (Row-Checked $list 0) -and (Row-Checked $list 1) } | Out-Null

        Step 'Disconnect the selected right disk while the checklist stays open'
        Detach-PickerImage $right
        Wait-For 'the removed member to disappear' { (Picker-Count $list) -eq 1 -and (Row-Checked $list 0) } | Out-Null
        Tab-To $picker $refresh
        Send-PickerKeys $picker ' '
        $status = [PackageUI]::GetDlgItem($picker,2104)
        Wait-For 'removed-member warning surviving a refresh' {
            ![PickerInput]::IsWindowEnabled($open) -and [PickerInput]::Text($status) -match 'checked drive is no longer available|checked drive changed|review'
        } | Out-Null
        Save-Window $picker '05-disconnected' -Desktop
        if ($images -contains 'replacement.vhd') {
            $replacement = Join-Path $FixtureDirectory 'replacement.vhd'
            Step 'Attach an unrelated same-size disk reusing the removed disk number'
            $replacementId = Attach-PickerImage $replacement
            if ($replacementId -ne $rightId) { throw 'The replacement did not reuse the removed disk number' }
            Wait-For 'the replacement to appear unselected' { (Picker-Count $list) -eq 2 -and (Row-Checked $list 0) -and !(Row-Checked $list 1) } | Out-Null
            Save-Window $picker '06-replacement-unselected' -Desktop
            Detach-PickerImage $replacement
            Wait-For 'removal of the replacement' { (Picker-Count $list) -eq 1 } | Out-Null
            $Report.replacementDisk = $replacementId
        }
        $rightId = Attach-PickerImage $right
        Wait-For 'the returned disk to require a fresh selection' { (Picker-Count $list) -eq 2 -and (Row-Checked $list 0) -and !(Row-Checked $list 1) } | Out-Null
        Tab-To $picker $list
        Send-PickerKeys $picker '{END} '
        Wait-For 'both current members to be checked' { (Row-Checked $list 0) -and (Row-Checked $list 1) } | Out-Null
        Save-Window $picker '07-ready' -Desktop
        Tab-To $picker $open
        Send-PickerKeys $picker '{ENTER}'
        Step 'Open both selected members and verify the restore against the Linux source'
        Test-PickerRestore
        $Report.memberDisks = @($leftId, $rightId)
    } finally {
        foreach ($path in $attached.ToArray()) { Detach-PickerImage $path }
    }
    foreach ($path in $imageHashes.Keys) {
        if ((Get-FileHash -LiteralPath $path).Hash -ne $imageHashes[$path]) { throw "Fixture changed: $path" }
    }
    $Report.sourceHashes = $imageHashes
}
Test-ConnectedDrivePicker

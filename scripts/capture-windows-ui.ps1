param(
    [string]$OutputDirectory = "artifacts\windows-ui"
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$Executable = Join-Path $Root "target\release\zfs-send-explore-windows.exe"
$Output = Join-Path $Root $OutputDirectory
$TempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
$Trace = Join-Path $TempRoot "zfse-ui-startup.txt"
$env:ZFSE_UI_TRACE = $Trace

if (-not (Test-Path $Executable)) {
    throw "Build the release Windows client before capturing screenshots: $Executable"
}

New-Item -ItemType Directory -Force $Output | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class ZfseWindowCapture {
    public delegate bool EnumWindowsCallback(IntPtr hwnd, IntPtr lparam);

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);

    [DllImport("user32.dll")]
    public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder text, int count);

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr lparam);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hwnd);

    public static IntPtr FindVisibleWindow(uint processId) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr lparam) {
            uint candidate;
            GetWindowThreadProcessId(hwnd, out candidate);
            if (candidate == processId && IsWindowVisible(hwnd)) {
                found = hwnd;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
"@

function Get-WindowTitle([IntPtr]$Window) {
    $Text = New-Object System.Text.StringBuilder 1024
    [ZfseWindowCapture]::GetWindowText($Window, $Text, $Text.Capacity) | Out-Null
    return $Text.ToString()
}

function Wait-MainWindow([System.Diagnostics.Process]$Process) {
    for ($Attempt = 0; $Attempt -lt 120; $Attempt++) {
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "The Windows client exited before creating a main window (exit code $($Process.ExitCode))."
        }
        if ($Process.MainWindowHandle -ne [IntPtr]::Zero) {
            return $Process.MainWindowHandle
        }
        $OwnedWindow = [ZfseWindowCapture]::FindVisibleWindow($Process.Id)
        if ($OwnedWindow -ne [IntPtr]::Zero) {
            Write-Host "Using process-owned visible window: $(Get-WindowTitle $OwnedWindow)"
            return $OwnedWindow
        }
        $Foreground = [ZfseWindowCapture]::GetForegroundWindow()
        $ForegroundProcess = [uint32]0
        if ($Foreground -ne [IntPtr]::Zero) {
            [ZfseWindowCapture]::GetWindowThreadProcessId($Foreground, [ref]$ForegroundProcess) | Out-Null
            if ($ForegroundProcess -eq $Process.Id) {
                Write-Host "Using process-owned foreground window: $(Get-WindowTitle $Foreground)"
                return $Foreground
            }
        }
        Start-Sleep -Milliseconds 250
    }
    $Checkpoint = if (Test-Path $Trace) { Get-Content -Raw $Trace } else { "no startup checkpoint" }
    throw "The Windows client did not create a main window. Last checkpoint: $Checkpoint"
}

function Save-Window([IntPtr]$Window, [string]$Path) {
    $Bounds = New-Object ZfseWindowCapture+RECT
    if (-not [ZfseWindowCapture]::GetWindowRect($Window, [ref]$Bounds)) {
        throw "Could not measure window $Window."
    }
    $Width = $Bounds.Right - $Bounds.Left
    $Height = $Bounds.Bottom - $Bounds.Top
    $Bitmap = New-Object System.Drawing.Bitmap($Width, $Height)
    $Graphics = [System.Drawing.Graphics]::FromImage($Bitmap)
    $Device = $Graphics.GetHdc()
    try {
        if (-not [ZfseWindowCapture]::PrintWindow($Window, $Device, 2)) {
            throw "PrintWindow failed for $Window."
        }
    }
    finally {
        $Graphics.ReleaseHdc($Device)
        $Graphics.Dispose()
    }
    $Bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    $Bitmap.Dispose()
}

function Stop-Client([System.Diagnostics.Process]$Process) {
    if (-not $Process.HasExited) {
        $Process.CloseMainWindow() | Out-Null
        if (-not $Process.WaitForExit(5000)) {
            $Process.Kill()
            $Process.WaitForExit()
        }
    }
}

Write-Host "Capturing the welcome page"
$Welcome = Start-Process -FilePath $Executable -PassThru
try {
    $WelcomeWindow = Wait-MainWindow $Welcome
    Start-Sleep -Seconds 3
    Save-Window $WelcomeWindow (Join-Path $Output "welcome.png")
}
finally {
    Stop-Client $Welcome
}

$SendFixture = Join-Path $Root "tests\fixtures\multi-snapshot.zfs"
Write-Host "Capturing the source browser"
$Browser = Start-Process -FilePath $Executable -ArgumentList ('"' + $SendFixture + '"') -PassThru
try {
    $BrowserWindow = Wait-MainWindow $Browser
    Start-Sleep -Seconds 5
    Save-Window $BrowserWindow (Join-Path $Output "source-browser.png")
}
finally {
    Stop-Client $Browser
}

$EncryptedFixture = Join-Path $Root "tests\fixtures\encrypted-raw-s1.zfs"
Write-Host "Capturing the unlock flow"
$UnlockFlow = Start-Process -FilePath $Executable -ArgumentList ('"' + $EncryptedFixture + '"') -PassThru
try {
    $UnlockMainWindow = Wait-MainWindow $UnlockFlow
    Start-Sleep -Seconds 5
    $UnlockWindow = [ZfseWindowCapture]::GetForegroundWindow()
    if ($UnlockWindow -eq $UnlockMainWindow -or (Get-WindowTitle $UnlockWindow) -ne "ZFS Explore") {
        throw "Expected the unlock dialog, found '$(Get-WindowTitle $UnlockWindow)'."
    }
    Save-Window $UnlockWindow (Join-Path $Output "unlock.png")
    [ZfseWindowCapture]::SetForegroundWindow($UnlockWindow) | Out-Null
    # Public test-fixture passphrase from tests/desktop_service.rs.
    [System.Windows.Forms.SendKeys]::SendWait("zfs-send-fixture-passphrase{ENTER}")
    Start-Sleep -Seconds 4
    Save-Window $UnlockMainWindow (Join-Path $Output "encrypted-browse.png")
}
finally {
    Stop-Client $UnlockFlow
}

# ZFSE_CAPTURE_EXT4 may name an already decoded image (for machines without Python).
$StandaloneImage = $env:ZFSE_CAPTURE_EXT4
if (-not $StandaloneImage) {
    $Compressed = Join-Path $Root "tests\fixtures\inception\ext4.img.zst.b64"
    $StandaloneImage = Join-Path $TempRoot "zfse-ext4.img"
    $Decoder = @"
import base64
import io
import pathlib
import zstandard

source = pathlib.Path(r'$Compressed')
target = pathlib.Path(r'$StandaloneImage')
compressed = io.BytesIO(base64.b64decode(source.read_bytes()))
with zstandard.ZstdDecompressor().stream_reader(compressed) as reader:
    target.write_bytes(reader.read())
"@
    $Decoder | python -
    if ($LASTEXITCODE -ne 0) {
        throw "Could not decode the standalone image fixture."
    }
}

$ImageBrowser = Start-Process -FilePath $Executable -ArgumentList ('"' + $StandaloneImage + '"') -PassThru
Write-Host "Capturing the standalone image browser"
try {
    $ImageWindow = Wait-MainWindow $ImageBrowser
    Start-Sleep -Seconds 5
    Save-Window $ImageWindow (Join-Path $Output "standalone-image.png")
}
finally {
    Stop-Client $ImageBrowser
}

Write-Host "Captured native Windows UI screenshots in $Output"

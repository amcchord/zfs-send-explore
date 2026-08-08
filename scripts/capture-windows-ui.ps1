param(
    [string]$OutputDirectory = "artifacts\windows-ui"
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$Executable = Join-Path $Root "target\release\zfs-send-explore-windows.exe"
$Output = Join-Path $Root $OutputDirectory

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
}
"@

function Wait-MainWindow([System.Diagnostics.Process]$Process) {
    for ($Attempt = 0; $Attempt -lt 120; $Attempt++) {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne [IntPtr]::Zero) {
            return $Process.MainWindowHandle
        }
        Start-Sleep -Milliseconds 250
    }
    throw "The Windows client did not create a main window."
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

$SendFixture = Join-Path $Root "tests\fixtures\multi-snapshot.zfs"
$Browser = Start-Process -FilePath $Executable -ArgumentList ('"' + $SendFixture + '"') -PassThru
try {
    $BrowserWindow = Wait-MainWindow $Browser
    Start-Sleep -Seconds 5
    Save-Window $BrowserWindow (Join-Path $Output "source-browser.png")

    [ZfseWindowCapture]::SetForegroundWindow($BrowserWindow) | Out-Null
    [System.Windows.Forms.SendKeys]::SendWait("^k")
    Start-Sleep -Seconds 2
    $CredentialWindow = [ZfseWindowCapture]::GetForegroundWindow()
    Save-Window $CredentialWindow (Join-Path $Output "credential-entry.png")
    [System.Windows.Forms.SendKeys]::SendWait("{ESC}")
}
finally {
    Stop-Client $Browser
}

$Compressed = Join-Path $Root "tests\fixtures\inception\ext4.img.zst.b64"
$StandaloneImage = Join-Path $env:RUNNER_TEMP "zfse-ext4.img"
@"
import base64
import pathlib
import zstandard

source = pathlib.Path(r'$Compressed')
target = pathlib.Path(r'$StandaloneImage')
target.write_bytes(zstandard.ZstdDecompressor().decompress(base64.b64decode(source.read_bytes())))
"@ | python -

$ImageBrowser = Start-Process -FilePath $Executable -ArgumentList ('"' + $StandaloneImage + '"') -PassThru
try {
    $ImageWindow = Wait-MainWindow $ImageBrowser
    Start-Sleep -Seconds 5
    Save-Window $ImageWindow (Join-Path $Output "standalone-image.png")
}
finally {
    Stop-Client $ImageBrowser
}

Write-Host "Captured native Windows UI screenshots in $Output"

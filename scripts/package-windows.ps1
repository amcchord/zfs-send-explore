$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$Stage = Join-Path $Root "target\windows-package\zfs-send-explore-windows"
$Archive = Join-Path $Root "target\windows-package\zfs-send-explore-windows-x86_64.zip"

cargo build --release --locked --bin zfs-send-extract --bin zfs-send-explore-windows

if (Test-Path $Stage) {
    Remove-Item -Recurse -Force $Stage
}
New-Item -ItemType Directory -Force $Stage | Out-Null
$DocsStage = Join-Path $Stage "docs"
New-Item -ItemType Directory -Force $DocsStage | Out-Null
Copy-Item (Join-Path $Root "target\release\zfs-send-explore-windows.exe") $Stage
Copy-Item (Join-Path $Root "target\release\zfs-send-extract.exe") $Stage
Copy-Item (Join-Path $Root "README.md") $Stage
Copy-Item (Join-Path $Root "docs\windows-client.md") $DocsStage
Copy-Item (Join-Path $Root "docs\windows-ux-review.md") $DocsStage
Copy-Item (Join-Path $Root "docs\screenshots") (Join-Path $DocsStage "screenshots") -Recurse
Copy-Item (Join-Path $Root "LICENSE") $Stage

if (Test-Path $Archive) {
    Remove-Item -Force $Archive
}
Compress-Archive -Path (Join-Path $Stage "*") -DestinationPath $Archive
Write-Host "Created $Archive"

param(
    [string]$WorkerService = "launcher-restore-worker",
    [string]$StorageRoot = "/var/lib/launcher/storage",
    [int]$Bytes = 1048576,
    [switch]$Railway,
    [switch]$Local
)

. (Join-Path $PSScriptRoot "common.ps1")

if ($Railway -and $Local) { throw "Choose -Railway or -Local, not both" }
if (-not $Railway -and -not $Local) {
    $Local = $true
}

if ($Railway) {
    if (-not (Get-Command railway -ErrorAction SilentlyContinue)) {
        throw "Railway CLI is required for -Railway; use the Railway UI shell if it is unavailable"
    }
    Invoke-RailwayAdmin -Service $WorkerService -Arguments @(
        "storage", "telegram-smoke",
        "--storage-root", $StorageRoot,
        "--bytes", $Bytes
    )
}
else {
    Invoke-LauncherAdmin @(
        "storage", "telegram-smoke",
        "--storage-root", (Resolve-StagingPath "artifacts\staging-storage"),
        "--bytes", $Bytes
    )
}

Write-Output "telegram_smoke=PASS"

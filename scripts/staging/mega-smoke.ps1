param(
    [string]$WorkerService = "launcher-restore-worker",
    [string]$StorageRoot = "/var/lib/launcher/storage",
    [string]$LocalStorageRoot = "artifacts\staging-storage",
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
        "storage", "mega-smoke",
        "--storage-root", $StorageRoot
    )
}
else {
    Invoke-LauncherAdmin @("storage", "mega-smoke", "--storage-root", (Resolve-StagingPath $LocalStorageRoot))
}

Write-Output "mega_smoke=PASS"

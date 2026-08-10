param(
    [Parameter(Mandatory = $true)][string]$BuildId,
    [Parameter(Mandatory = $true)][string]$EncodedHash,
    [string]$WorkerService = "launcher-restore-worker",
    [string]$StorageRoot = "/var/lib/launcher/storage",
    [string]$LocalStorageRoot = "artifacts\staging-storage",
    [switch]$Railway,
    [switch]$Local,
    [switch]$Confirm
)

. (Join-Path $PSScriptRoot "common.ps1")

if (-not $Confirm) {
    throw "This test deletes one HOT object and restores it. Re-run with -Confirm after checking the staging build and hash."
}
if (-not $BuildId.StartsWith("staging-", [StringComparison]::OrdinalIgnoreCase)) {
    throw "Cold restore test only accepts build IDs beginning with staging-"
}
if ($EncodedHash -cnotmatch "^[0-9a-f]{64}$") {
    throw "EncodedHash must be a 64-character lowercase BLAKE3 hash"
}
if ($Railway -and $Local) { throw "Choose -Railway or -Local, not both" }
if (-not $Railway -and -not $Local) { $Railway = $true }

$arguments = @(
    "storage", "cold-restore-smoke",
    "--build-id", $BuildId,
    "--encoded-hash", $EncodedHash,
    "--confirm",
    "--storage-root", $StorageRoot
)
if ($Railway) {
    if (-not (Get-Command railway -ErrorAction SilentlyContinue)) {
        throw "Railway CLI is required for -Railway; use the Railway SSH command from the dashboard"
    }
    Invoke-RailwayAdmin -Service $WorkerService -Arguments $arguments
}
else {
    $localArguments = @(
        "storage", "cold-restore-smoke",
        "--build-id", $BuildId,
        "--encoded-hash", $EncodedHash,
        "--confirm",
        "--storage-root", (Resolve-StagingPath $LocalStorageRoot)
    )
    Invoke-LauncherAdmin $localArguments
}

Write-Output "cold_restore_test=PASS"

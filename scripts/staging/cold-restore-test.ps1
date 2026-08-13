param(
    [Parameter(Mandatory = $true)][string]$BuildId,
    [Parameter(Mandatory = $true)][string]$EncodedHash,
    [string]$WorkerService = "worker",
    [string]$StorageRoot = "/var/lib/launcher/storage",
    [string]$LocalStorageRoot = "artifacts\staging-storage",
    [switch]$Mantle,
    [switch]$Local,
    [switch]$Confirm,
    [string]$RemoteHost,
    [string]$RemoteUser = "debian",
    [string]$IdentityFile,
    [string]$RemoteDirectory = "/home/debian/vaultnode"
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
if ($Mantle -and $Local) { throw "Choose -Mantle or -Local, not both" }
if (-not $Mantle -and -not $Local) { $Local = $true }

$arguments = @(
    "storage", "cold-restore-smoke",
    "--build-id", $BuildId,
    "--encoded-hash", $EncodedHash,
    "--confirm",
    "--storage-root", $StorageRoot
)
if ($Mantle) {
    if ([string]::IsNullOrWhiteSpace($RemoteHost)) {
        throw "-RemoteHost is required with -Mantle"
    }
    if ([string]::IsNullOrWhiteSpace($IdentityFile)) {
        throw "-IdentityFile is required with -Mantle"
    }
    Invoke-MantleAdmin -RemoteHost $RemoteHost -RemoteUser $RemoteUser `
        -IdentityFile $IdentityFile -RemoteDirectory $RemoteDirectory `
        -Service $WorkerService -Arguments $arguments
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

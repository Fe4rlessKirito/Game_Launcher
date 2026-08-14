param(
    [Parameter(Mandatory = $true)][string]$BuildId,
    [string]$EncodedHash,
    [string]$PackHash,
    [switch]$MetadataOnly,
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
    throw "This test evicts one HOT reference and restores it. With -MetadataOnly, the provider-side object is not deleted; otherwise the provider delete capability must be proven. Re-run with -Confirm after checking the staging build and hash."
}
if (-not $BuildId.StartsWith("staging-", [StringComparison]::OrdinalIgnoreCase)) {
    throw "Cold restore test only accepts build IDs beginning with staging-"
}
if ([string]::IsNullOrWhiteSpace($EncodedHash) -eq [string]::IsNullOrWhiteSpace($PackHash)) {
    throw "Provide exactly one of -EncodedHash (legacy logical-object smoke) or -PackHash (physical-pack smoke)"
}
if ($EncodedHash -and $EncodedHash -cnotmatch "^[0-9a-f]{64}$") {
    throw "EncodedHash must be a 64-character lowercase BLAKE3 hash"
}
if ($PackHash -and $PackHash -cnotmatch "^[0-9a-f]{64}$") {
    throw "PackHash must be a 64-character lowercase BLAKE3 hash"
}
if ($MetadataOnly -and -not $PackHash) {
    throw "-MetadataOnly is only valid with -PackHash"
}
if ($Mantle -and $Local) { throw "Choose -Mantle or -Local, not both" }
if (-not $Mantle -and -not $Local) { $Local = $true }

if ($PackHash) {
    $arguments = @(
        "storage", "cold-pack-restore-smoke",
        "--build-id", $BuildId,
        "--pack-hash", $PackHash,
        "--confirm",
        "--storage-root", $StorageRoot
    )
    if ($MetadataOnly) {
        $arguments += "--metadata-only"
    }
}
else {
    $arguments = @(
        "storage", "cold-restore-smoke",
        "--build-id", $BuildId,
        "--encoded-hash", $EncodedHash,
        "--confirm",
        "--storage-root", $StorageRoot
    )
}
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
        $arguments
    )
    $localArguments[-1] = (Resolve-StagingPath $LocalStorageRoot)
    Invoke-LauncherAdmin $localArguments
}

Write-Output "cold_restore_test=PASS"

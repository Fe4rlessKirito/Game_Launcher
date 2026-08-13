param(
    [string]$WorkerService = "worker",
    [string]$StorageRoot = "/var/lib/launcher/storage",
    [int]$Bytes = 1048576,
    [switch]$Mantle,
    [switch]$Local,
    [string]$RemoteHost,
    [string]$RemoteUser = "debian",
    [string]$IdentityFile,
    [string]$RemoteDirectory = "/home/debian/vaultnode"
)

. (Join-Path $PSScriptRoot "common.ps1")

if ($Mantle -and $Local) { throw "Choose -Mantle or -Local, not both" }
if (-not $Mantle -and -not $Local) {
    $Local = $true
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
        -Service $WorkerService -Arguments @(
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

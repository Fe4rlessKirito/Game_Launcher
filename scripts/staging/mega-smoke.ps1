param(
    [string]$LocalStorageRoot = "artifacts\staging-storage"
)

. (Join-Path $PSScriptRoot "common.ps1")

Invoke-LauncherAdmin @("storage", "mega-smoke", "--storage-root", (Resolve-StagingPath $LocalStorageRoot))

Write-Output "mega_smoke=PASS"

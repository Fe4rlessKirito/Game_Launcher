param(
    [string]$ApiUrl = $env:LAUNCHER_STAGING_API_URL,
    [string]$ManifestBuildId,
    [string]$TrustedPublicKeyPath,
    [string]$ExpectedKeyId = "staging-2026-01",
    [switch]$RequireCold,
    [switch]$AllowHttp
)

. (Join-Path $PSScriptRoot "common.ps1")

if ([string]::IsNullOrWhiteSpace($ApiUrl)) {
    throw "Provide -ApiUrl or set LAUNCHER_STAGING_API_URL"
}

$arguments = @("staging", "verify", "--api-url", $ApiUrl, "--expected-key-id", $ExpectedKeyId)
if ($RequireCold) { $arguments += "--require-cold" }
if ($AllowHttp) { $arguments += "--allow-http" }
if (-not [string]::IsNullOrWhiteSpace($ManifestBuildId)) {
    if ([string]::IsNullOrWhiteSpace($TrustedPublicKeyPath)) {
        throw "-TrustedPublicKeyPath is required with -ManifestBuildId"
    }
    $publicKey = [IO.Path]::GetFullPath($TrustedPublicKeyPath)
    if (-not (Test-Path -LiteralPath $publicKey)) {
        throw "Trusted public key does not exist: $publicKey"
    }
    $arguments += @("--manifest-build-id", $ManifestBuildId, "--trusted-public-key", $publicKey)
}

Invoke-LauncherAdmin $arguments

if (-not [string]::IsNullOrWhiteSpace($env:DATABASE_URL)) {
    Invoke-LauncherAdmin @("db", "status")
}
else {
    Write-Output "db_status=SKIPPED DATABASE_URL is not present in this shell; API readiness already checked the database dependency"
}

Write-Output "verify_staging=PASS"

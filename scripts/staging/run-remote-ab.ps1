param(
    [Parameter(Mandatory = $true)][string]$ApiUrl,
    [Parameter(Mandatory = $true)][string]$SettingsPath,
    [Parameter(Mandatory = $true)][string]$SourceA,
    [Parameter(Mandatory = $true)][string]$SourceB,
    [string]$BuildAId = "staging-a",
    [string]$BuildBId = "staging-b",
    [string]$StateRoot = "artifacts\staging-remote\state",
    [string]$InstallRoot = "artifacts\staging-remote\installed",
    [switch]$SkipVerify
)

. (Join-Path $PSScriptRoot "common.ps1")

$sourceAPath = [IO.Path]::GetFullPath($SourceA)
$sourceBPath = [IO.Path]::GetFullPath($SourceB)
$settingsPathFull = [IO.Path]::GetFullPath($SettingsPath)
if (-not (Test-Path -LiteralPath $sourceAPath -PathType Container)) { throw "Build A source directory does not exist: $sourceAPath" }
if (-not (Test-Path -LiteralPath $sourceBPath -PathType Container)) { throw "Build B source directory does not exist: $sourceBPath" }
if (-not (Test-Path -LiteralPath $settingsPathFull -PathType Leaf)) { throw "Launcher settings file does not exist: $settingsPathFull" }
$statePath = Assert-ArtifactPath (Resolve-StagingPath $StateRoot)
$installPath = Assert-ArtifactPath (Resolve-StagingPath $InstallRoot)

if (-not $SkipVerify) {
    $verifyScript = Join-Path $PSScriptRoot "verify-staging.ps1"
    Invoke-Checked -File "powershell" -Arguments @(
        "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", $verifyScript,
        "-ApiUrl", $ApiUrl
    )
}

$dotnet = Get-LauncherDotnet
$runner = Join-Path $script:StagingRepoRoot "launcher\src\Launcher.E2E\bin\Release\net10.0\Launcher.E2E.dll"
if (-not (Test-Path -LiteralPath $runner)) {
    Invoke-Checked -File $dotnet -Arguments @(
        "build",
        (Join-Path $script:StagingRepoRoot "launcher\Launcher.sln"),
        "--configuration", "Release",
        "--no-restore"
    )
}
if (-not (Test-Path -LiteralPath $runner)) { throw "Launcher E2E runner was not built: $runner" }

if (Test-Path -LiteralPath $statePath) { Remove-Item -LiteralPath $statePath -Recurse -Force }
if (Test-Path -LiteralPath $installPath) { Remove-Item -LiteralPath $installPath -Recurse -Force }
New-Item -ItemType Directory -Path $statePath, $installPath | Out-Null

function Invoke-LauncherE2EPhase {
    param(
        [Parameter(Mandatory = $true)][string]$Mode,
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$BuildId
    )

    $keys = @(
        "LAUNCHER_E2E_MODE",
        "LAUNCHER_E2E_API",
        "LAUNCHER_E2E_STATE_ROOT",
        "LAUNCHER_E2E_INSTALL_ROOT",
        "LAUNCHER_E2E_SOURCE",
        "LAUNCHER_E2E_BUILD_ID",
        "LAUNCHER_SETTINGS_PATH"
    )
    $previous = @{}
    foreach ($key in $keys) {
        $previous[$key] = [Environment]::GetEnvironmentVariable($key, "Process")
    }
    try {
        $env:LAUNCHER_E2E_MODE = $Mode
        $env:LAUNCHER_E2E_API = $ApiUrl.TrimEnd("/") + "/"
        $env:LAUNCHER_E2E_STATE_ROOT = $statePath
        $env:LAUNCHER_E2E_INSTALL_ROOT = $installPath
        $env:LAUNCHER_E2E_SOURCE = $Source
        $env:LAUNCHER_E2E_BUILD_ID = $BuildId
        $env:LAUNCHER_SETTINGS_PATH = $settingsPathFull
        $lines = & $dotnet $runner 2>&1
        $exitCode = $LASTEXITCODE
        foreach ($line in $lines) { Write-Host $line }
        if ($exitCode -ne 0) {
            throw "Launcher E2E phase $Mode failed with exit code $exitCode"
        }
        $jsonLine = $lines |
            Where-Object { $_ -is [string] -and $_.TrimStart().StartsWith("{") } |
            Select-Object -Last 1
        if ([string]::IsNullOrWhiteSpace($jsonLine)) {
            throw "Launcher E2E phase $Mode did not emit a JSON result"
        }
        return ($jsonLine | ConvertFrom-Json)
    }
    finally {
        foreach ($key in $keys) {
            [Environment]::SetEnvironmentVariable($key, $previous[$key], "Process")
        }
    }
}

$installResult = Invoke-LauncherE2EPhase -Mode "install" -Source $sourceAPath -BuildId $BuildAId
$updateResult = Invoke-LauncherE2EPhase -Mode "update" -Source $sourceBPath -BuildId $BuildBId

$baseUri = [Uri]($ApiUrl.TrimEnd("/") + "/")
$escapedBuild = [Uri]::EscapeDataString($BuildBId)
$manifest = Invoke-RestMethod -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/manifest"))
$firstChunk = @($manifest.files | ForEach-Object { $_.chunks } | Select-Object -First 1)
if ($null -eq $firstChunk -or $firstChunk.Count -eq 0) { throw "Build B manifest did not contain a chunk" }
$resolveBody = @{ encoded_hashes = @($firstChunk[0].encoded_hash) } | ConvertTo-Json -Compress
$resolved = Invoke-RestMethod -Method Post -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/resolve")) -ContentType "application/json" -Body $resolveBody
$directUrl = ([Uri](@($resolved)[0].urls[0]))
if ($directUrl.Host -eq $baseUri.Host) {
    throw "Resolved chunk URL uses the API host; staging data-plane routing is not direct-to-bucket"
}

Write-Output "remote_ab=PASS"
Write-Output "data_plane=PASS direct_host=$($directUrl.Host)"
Write-Output "build_a_id=$BuildAId"
Write-Output "build_a_encoded_bytes=$([int64]$installResult.total_encoded_bytes)"
Write-Output "build_b_id=$BuildBId"
Write-Output "build_b_encoded_bytes=$([int64]$updateResult.total_encoded_bytes)"
Write-Output "network_downloaded_bytes=$([int64]$updateResult.network_bytes)"
Write-Output "local_cache_reuse_bytes=$([int64]$updateResult.reused_cache_bytes)"
Write-Output "savings_percent=$([Math]::Round([double]$updateResult.network_savings * 100, 4))"
Write-Output "byte_identity=PASS"

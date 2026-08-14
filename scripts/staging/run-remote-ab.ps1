param(
    [Parameter(Mandatory = $true)][string]$ApiUrl,
    [Parameter(Mandatory = $true)][string]$SettingsPath,
    [Parameter(Mandatory = $true)][string]$SourceA,
    [Parameter(Mandatory = $true)][string]$SourceB,
    [string]$BuildAId = "staging-a",
    [string]$BuildBId = "staging-b",
    [string]$StateRoot = "artifacts\staging-remote\state",
    [string]$InstallRoot = "artifacts\staging-remote\installed",
    [switch]$SkipVerify,
    [switch]$SkipLaunch
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
        "LAUNCHER_E2E_SKIP_LAUNCH",
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
        if ($SkipLaunch) { $env:LAUNCHER_E2E_SKIP_LAUNCH = "true" }
        else { Remove-Item Env:LAUNCHER_E2E_SKIP_LAUNCH -ErrorAction SilentlyContinue }
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

# Deliberately damage the installed B build, then let the launcher repair it
# from the verified manifest. This stays inside the staging artifact root.
function Damage-InstalledBuild {
    $syntheticChanged = Join-Path $installPath "Data\changed.txt"
    $syntheticAdded = Join-Path $installPath "Data\added.bin"
    $syntheticInserted = Join-Path $installPath "Data\inserted.bin"
    if (Test-Path -LiteralPath $syntheticChanged -PathType Leaf) {
        Set-Content -LiteralPath $syntheticChanged -Value "staging-corruption" -NoNewline -Encoding UTF8
        if (Test-Path -LiteralPath $syntheticAdded -PathType Leaf) {
            Remove-Item -LiteralPath $syntheticAdded -Force
        }
        if (Test-Path -LiteralPath $syntheticInserted -PathType Leaf) {
            $truncateStream = [IO.File]::Open($syntheticInserted, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $truncateStream.SetLength([Math]::Min(1024, $truncateStream.Length)) } finally { $truncateStream.Dispose() }
        }
        return
    }

    # Real-game validation does not assume a particular fixture layout. Damage
    # one non-empty installed file and remove another file when available.
    $candidates = @(Get-ChildItem -LiteralPath $installPath -File -Recurse | Sort-Object FullName)
    $nonEmpty = @($candidates | Where-Object Length -gt 0 | Select-Object -First 1)
    if ($nonEmpty.Count -eq 0) { throw "Installed build contains no non-empty file to damage." }
    $stream = [IO.File]::Open($nonEmpty[0].FullName, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $stream.Position = 0
        $stream.WriteByte(0xA5)
    }
    finally { $stream.Dispose() }
    $second = $candidates | Where-Object { $_.FullName -ne $nonEmpty[0].FullName } | Select-Object -First 1
    if ($null -ne $second) { Remove-Item -LiteralPath $second.FullName -Force }
}

Damage-InstalledBuild
$repairResult = Invoke-LauncherE2EPhase -Mode "repair" -Source $sourceBPath -BuildId $BuildBId

$baseUri = [Uri]($ApiUrl.TrimEnd("/") + "/")
$escapedBuild = [Uri]::EscapeDataString($BuildBId)
$manifest = Invoke-RestMethod -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/manifest"))
$firstChunk = @($manifest.files | ForEach-Object { $_.chunks } | Select-Object -First 1)
if ($null -eq $firstChunk -or $firstChunk.Count -eq 0) { throw "Build B manifest did not contain a chunk" }
$resolveBody = @{ encoded_hashes = @($firstChunk[0].encoded_hash) } | ConvertTo-Json -Compress
$packResolved = Invoke-RestMethod -Method Post -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/packs/resolve")) -ContentType "application/json" -Body $resolveBody
$directUrl = $null
foreach ($pack in @($packResolved)) {
    foreach ($source in @($pack.sources)) {
        if (-not [string]::IsNullOrWhiteSpace($source.url)) {
            $directUrl = [Uri]$source.url
            break
        }
    }
    if ($null -ne $directUrl) { break }
}
if ($null -eq $directUrl) {
    $legacyResolved = Invoke-RestMethod -Method Post -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/resolve")) -ContentType "application/json" -Body $resolveBody
    $directUrl = ([Uri](@($legacyResolved)[0].urls[0]))
}
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
Write-Output "repair_network_bytes=$([int64]$repairResult.network_bytes)"
Write-Output "byte_identity=PASS"
Write-Output "repair_byte_identity=PASS"

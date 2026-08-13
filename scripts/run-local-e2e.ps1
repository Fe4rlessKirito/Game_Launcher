param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"
$overallStopwatch = [Diagnostics.Stopwatch]::StartNew()
$repoRoot = Split-Path $PSScriptRoot -Parent
$artifactsRoot = Join-Path $repoRoot "artifacts\e2e"
$fixtureRoot = Join-Path $artifactsRoot "synthetic-game"
$runRoot = Join-Path $artifactsRoot "run"
$catalogRoot = Join-Path $runRoot "catalog"
$storageRoot = Join-Path $runRoot "storage"
$stateRoot = Join-Path $runRoot "launcher-state"
$installRoot = Join-Path $runRoot "installed-game"
$apiLog = Join-Path $runRoot "api.log"
$apiErrorLog = Join-Path $runRoot "api.error.log"
$expectedArtifacts = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts")) + [IO.Path]::DirectorySeparatorChar
$runRootFull = [IO.Path]::GetFullPath($runRoot)
if (-not $runRootFull.StartsWith($expectedArtifacts, [StringComparison]::OrdinalIgnoreCase)) { throw "E2E output escaped artifacts directory" }
if (Test-Path -LiteralPath $runRoot) { Remove-Item -LiteralPath $runRoot -Recurse -Force }
New-Item -ItemType Directory -Path $runRoot, $catalogRoot, $storageRoot | Out-Null

$dotnet = Join-Path $repoRoot ".dotnet\dotnet.exe"
if (-not (Test-Path -LiteralPath $dotnet)) { $dotnet = "dotnet" }
if (Test-Path -LiteralPath $dotnet) { $env:DOTNET_ROOT = Split-Path $dotnet -Parent }
$cargo = "cargo"
$env:PYTHONPATH = Join-Path $repoRoot "analyzer\src"

function Run-Checked([string]$File, [string[]]$Arguments, [string]$WorkingDirectory = $repoRoot) {
    Push-Location $WorkingDirectory
    try {
        & $File @Arguments
        if ($LASTEXITCODE -ne 0) { throw "$File $($Arguments -join ' ') failed with exit code $LASTEXITCODE" }
    }
    finally { Pop-Location }
}

if (-not $SkipBuild) {
    Run-Checked $dotnet @("build", (Join-Path $repoRoot "launcher\Launcher.sln"), "--configuration", "Release", "--no-restore")
    Run-Checked $cargo @("build", "--manifest-path", (Join-Path $repoRoot "server\Cargo.toml"), "--workspace")
}

$generator = Join-Path $repoRoot "scripts\generate-synthetic-game.ps1"
Run-Checked "powershell" @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $generator, "-OutputRoot", $fixtureRoot)
$admin = Join-Path $repoRoot "server\target\debug\launcher-admin.exe"
$apiBinary = Join-Path $repoRoot "server\target\debug\launcher-api.exe"
$packageA = Join-Path $runRoot "package-A"
$packageB = Join-Path $runRoot "package-B"
$signatureTool = Join-Path $repoRoot "launcher\src\Launcher.SignatureTool\bin\Release\net10.0\Launcher.SignatureTool.dll"
$commonPackagerArgs = @("--minimum-bytes", "65536", "--average-bytes", "262144", "--maximum-bytes", "1048576")
Run-Checked $admin (@("ingest", (Join-Path $fixtureRoot "A"), "--output", $packageA, "--game-id", "synthetic-game", "--build-id", "build-a", "--display-version", "A", "--executable", "SyntheticGame.exe") + $commonPackagerArgs)
Run-Checked $admin @("manifest-sign", (Join-Path $packageA "manifest.json"), "--output", (Join-Path $packageA "manifest.sig.json"), "--key-id", "local-e2e-key")
Run-Checked $dotnet @($signatureTool, "verify", (Join-Path $packageA "manifest.json"), (Join-Path $packageA "manifest.sig.json"))
$mutatedManifest = Join-Path $runRoot "manifest-mutated.json"
$mutatedBytes = [IO.File]::ReadAllBytes((Join-Path $packageA "manifest.json"))
$mutatedBytes[0] = $mutatedBytes[0] -bxor 1
[IO.File]::WriteAllBytes($mutatedManifest, $mutatedBytes)
& $dotnet $signatureTool verify $mutatedManifest (Join-Path $packageA "manifest.sig.json") | Out-Null
if ($LASTEXITCODE -eq 0) { throw "Mutated Rust-signed manifest was accepted by the C# verifier" }
Run-Checked $admin (@("ingest", (Join-Path $fixtureRoot "B"), "--output", $packageB, "--game-id", "synthetic-game", "--build-id", "build-b", "--display-version", "B", "--executable", "SyntheticGame.exe") + $commonPackagerArgs)
Run-Checked $admin @("manifest-sign", (Join-Path $packageB "manifest.json"), "--output", (Join-Path $packageB "manifest.sig.json"), "--key-id", "local-e2e-key")
Run-Checked $admin @("publish", $packageA, "--catalog-root", $catalogRoot, "--storage-root", $storageRoot)

$env:LAUNCHER_STORAGE_ROOT = $storageRoot
$env:LAUNCHER_CATALOG_ROOT = $catalogRoot
$env:LAUNCHER_PUBLIC_BASE_URL = "http://127.0.0.1:18081"
$env:LAUNCHER_BIND = "127.0.0.1:18081"
$env:RUST_LOG = "info"
Remove-Item Env:DATABASE_URL -ErrorAction SilentlyContinue
if (Test-Path -LiteralPath $apiLog) { Remove-Item -LiteralPath $apiLog -Force }
if (Test-Path -LiteralPath $apiErrorLog) { Remove-Item -LiteralPath $apiErrorLog -Force }

function Start-LocalApi([string]$OutputLog, [string]$ErrorLog) {
    $process = Start-Process -FilePath $apiBinary -WorkingDirectory $repoRoot -PassThru -RedirectStandardOutput $OutputLog -RedirectStandardError $ErrorLog -WindowStyle Hidden
    for ($attempt = 0; $attempt -lt 50; $attempt++) {
        Start-Sleep -Milliseconds 200
        try {
            if ((Invoke-RestMethod "http://127.0.0.1:18081/health").status -eq "ok") { return $process }
        }
        catch { }
        if ($process.HasExited) { throw "Local API exited during startup. See $ErrorLog" }
    }
    throw "Local API did not become healthy. See $ErrorLog"
}

function Stop-LocalApi($process) {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit()
    }
}

$apiProcess = $null
try {
    $apiStartupStopwatch = [Diagnostics.Stopwatch]::StartNew()
    $apiProcess = Start-LocalApi $apiLog $apiErrorLog
    $apiStartupStopwatch.Stop()

    $runner = Join-Path $repoRoot "launcher\src\Launcher.E2E\bin\Release\net10.0\Launcher.E2E.dll"
    function Run-E2E([string]$Mode, [string]$Source, [string]$BuildId = "") {
        $env:LAUNCHER_E2E_MODE = $Mode
        $env:LAUNCHER_E2E_API = "http://127.0.0.1:18081/"
        $env:LAUNCHER_E2E_STATE_ROOT = $stateRoot
        $env:LAUNCHER_E2E_INSTALL_ROOT = $installRoot
        $env:LAUNCHER_E2E_SOURCE = $Source
        if ([String]::IsNullOrEmpty($BuildId)) { Remove-Item Env:LAUNCHER_E2E_BUILD_ID -ErrorAction SilentlyContinue } else { $env:LAUNCHER_E2E_BUILD_ID = $BuildId }
        $phaseStopwatch = [Diagnostics.Stopwatch]::StartNew()
        $lines = @(& $dotnet $runner 2>&1 | ForEach-Object { $_.ToString() })
        $phaseStopwatch.Stop()
        if ($LASTEXITCODE -ne 0) { throw "Launcher E2E phase $Mode failed:`n$($lines -join [Environment]::NewLine)" }
        $json = $lines | Where-Object { $_ -match '^\s*\{' } | Select-Object -Last 1
        if ([String]::IsNullOrWhiteSpace($json)) { throw "Launcher E2E phase $Mode emitted no JSON result" }
        $result = $json | ConvertFrom-Json
        $result | Add-Member -NotePropertyName elapsed_ms -NotePropertyValue ([Math]::Round($phaseStopwatch.Elapsed.TotalMilliseconds, 3))
        return $result
    }

    $installMetrics = Run-E2E "install" (Join-Path $fixtureRoot "A") "build-a"
    if (-not (Test-Path -LiteralPath (Join-Path $installRoot "launched.txt"))) { throw "Synthetic game did not launch" }

    Stop-LocalApi $apiProcess
    $apiProcess = $null
    Run-Checked $admin @("publish", $packageB, "--catalog-root", $catalogRoot, "--storage-root", $storageRoot)
    $apiProcess = Start-LocalApi (Join-Path $runRoot "api-update.log") (Join-Path $runRoot "api-update.error.log")
    $updateMetrics = Run-E2E "update" (Join-Path $fixtureRoot "B")

    $changedPath = Join-Path $installRoot "Data\changed.txt"
    Set-Content -LiteralPath $changedPath -Value "corrupted" -NoNewline -Encoding UTF8
    Remove-Item -LiteralPath (Join-Path $installRoot "Data\added.bin") -Force
    $truncatePath = Join-Path $installRoot "Data\inserted.bin"
    $truncateStream = [IO.File]::Open($truncatePath, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $truncateStream.SetLength(1024) } finally { $truncateStream.Dispose() }
    $repairMetrics = Run-E2E "repair" (Join-Path $fixtureRoot "B")

    $manifestA = Get-Content -Raw -LiteralPath (Join-Path $packageA "manifest.json") | ConvertFrom-Json
    $manifestB = Get-Content -Raw -LiteralPath (Join-Path $packageB "manifest.json") | ConvertFrom-Json
    $reportA = Get-Content -Raw -LiteralPath (Join-Path $packageA "report.json") | ConvertFrom-Json
    $reportB = Get-Content -Raw -LiteralPath (Join-Path $packageB "report.json") | ConvertFrom-Json
    $catalogTimer = [Diagnostics.Stopwatch]::StartNew()
    [void](Invoke-RestMethod "http://127.0.0.1:18081/api/v1/games?limit=24")
    $catalogTimer.Stop()
    $manifestTimer = [Diagnostics.Stopwatch]::StartNew()
    [void](Invoke-RestMethod "http://127.0.0.1:18081/api/v1/builds/build-b/manifest")
    $manifestTimer.Stop()
    $resolveHashes = @($manifestB.files | ForEach-Object { $_.chunks } | Select-Object -First 4 | ForEach-Object { $_.encoded_hash })
    $resolveBody = @{ encoded_hashes = $resolveHashes } | ConvertTo-Json -Compress
    $resolveTimer = [Diagnostics.Stopwatch]::StartNew()
    [void](Invoke-RestMethod "http://127.0.0.1:18081/api/v1/builds/build-b/resolve" -Method Post -ContentType "application/json" -Body $resolveBody)
    $resolveTimer.Stop()
    $aHashes = @{}
    foreach ($file in $manifestA.files) { foreach ($chunk in $file.chunks) { $aHashes[$chunk.encoded_hash] = $chunk.encoded_size } }
    $reusableEncoded = [int64]0
    foreach ($file in $manifestB.files) { foreach ($chunk in $file.chunks) { if ($aHashes.ContainsKey($chunk.encoded_hash)) { $reusableEncoded += [int64]$chunk.encoded_size } } }

    function Get-FixedHashes([byte[]]$Bytes, [int]$Size) {
        $hashes = @()
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
        for ($offset = 0; $offset -lt $Bytes.Length; $offset += $Size) {
            $length = [Math]::Min($Size, $Bytes.Length - $offset)
            $block = New-Object byte[] $length
            [Array]::Copy($Bytes, $offset, $block, 0, $length)
            $hashes += ([BitConverter]::ToString($sha.ComputeHash($block)).Replace('-', '')).ToLowerInvariant()
        }
        }
        finally { $sha.Dispose() }
        return $hashes
    }
    $sourceAInserted = [IO.File]::ReadAllBytes((Join-Path $fixtureRoot "A\Data\inserted.bin"))
    $sourceBInserted = [IO.File]::ReadAllBytes((Join-Path $fixtureRoot "B\Data\inserted.bin"))
    $fixedA = Get-FixedHashes $sourceAInserted 262144
    $fixedB = Get-FixedHashes $sourceBInserted 262144
    $fixedReusable = ($fixedB | Where-Object { $fixedA -contains $_ }).Count
    $fastA = ($manifestA.files | Where-Object path -eq "Data/inserted.bin").chunks.raw_hash
    $fastB = ($manifestB.files | Where-Object path -eq "Data/inserted.bin").chunks.raw_hash
    $fastReusable = ($fastB | Where-Object { $fastA -contains $_ }).Count

    $metrics = [pscustomobject]@{
        build_a_raw_bytes = [int64]$reportA.raw_bytes
        build_a_encoded_bytes = [int64]$reportA.encoded_bytes
        build_b_raw_bytes = [int64]$reportB.raw_bytes
        build_b_encoded_bytes = [int64]$reportB.encoded_bytes
        build_b_network_bytes = [int64]$updateMetrics.network_bytes
        build_b_cache_reused_bytes = [int64]$updateMetrics.reused_cache_bytes
        build_b_reusable_encoded_bytes = $reusableEncoded
        build_b_network_savings = $updateMetrics.network_savings
        build_b_reused_installed_bytes = [int64]$updateMetrics.reused_installed_bytes
        build_b_reconstructed_bytes = [int64]$updateMetrics.reconstructed_bytes
        repair_network_bytes = [int64]$repairMetrics.network_bytes
        fixed_size_reusable_chunks = $fixedReusable
        fastcdc_reusable_chunks = $fastReusable
        api_startup_ms = [Math]::Round($apiStartupStopwatch.Elapsed.TotalMilliseconds, 3)
        api_catalog_ms = [Math]::Round($catalogTimer.Elapsed.TotalMilliseconds, 3)
        api_manifest_ms = [Math]::Round($manifestTimer.Elapsed.TotalMilliseconds, 3)
        api_resolve_ms = [Math]::Round($resolveTimer.Elapsed.TotalMilliseconds, 3)
        total_elapsed_ms = [Math]::Round($overallStopwatch.Elapsed.TotalMilliseconds, 3)
        install = $installMetrics
        update = $updateMetrics
        repair = $repairMetrics
    }
    $metricsPath = Join-Path $runRoot "metrics.json"
    $metrics | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $metricsPath -Encoding UTF8
    Write-Output "E2E metrics: $metricsPath"
    $metrics | ConvertTo-Json -Depth 4
}
finally {
    Stop-LocalApi $apiProcess
    Remove-Item Env:LAUNCHER_E2E_MODE -ErrorAction SilentlyContinue
    Remove-Item Env:LAUNCHER_E2E_API -ErrorAction SilentlyContinue
    Remove-Item Env:LAUNCHER_E2E_STATE_ROOT -ErrorAction SilentlyContinue
    Remove-Item Env:LAUNCHER_E2E_INSTALL_ROOT -ErrorAction SilentlyContinue
    Remove-Item Env:LAUNCHER_E2E_SOURCE -ErrorAction SilentlyContinue
    Remove-Item Env:LAUNCHER_E2E_BUILD_ID -ErrorAction SilentlyContinue
}

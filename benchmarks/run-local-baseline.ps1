param([int]$SizeMiB = 512)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path $PSScriptRoot -Parent
$outputRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts\performance-baseline"))
$artifactsRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts")) + [IO.Path]::DirectorySeparatorChar
if (-not $outputRoot.StartsWith($artifactsRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Benchmark output escaped artifacts" }
if ($SizeMiB -lt 128 -or $SizeMiB -gt 4096) { throw "SizeMiB must be between 128 and 4096" }
if (Test-Path -LiteralPath $outputRoot) { Remove-Item -LiteralPath $outputRoot -Recurse -Force }
$inputRoot = Join-Path $outputRoot "input"
$packageRoot = Join-Path $outputRoot "package"
$throughputRoot = Join-Path $outputRoot "throughput"
New-Item -ItemType Directory -Path $inputRoot, $throughputRoot | Out-Null

function New-DeterministicFile([string]$Path, [int64]$Length, [uint32]$Seed) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $buffer = New-Object byte[] (1024 * 1024)
        $state = $Seed
        $remaining = $Length
        while ($remaining -gt 0) {
            $count = [Math]::Min($buffer.Length, $remaining)
            for ($index = 0; $index -lt $count; $index++) {
                $state = $state -bxor (($state -shl 13) -band 0xffffffff)
                $state = $state -bxor (($state -shr 17) -band 0xffffffff)
                $state = $state -bxor (($state -shl 5) -band 0xffffffff)
                $buffer[$index] = [byte]($state -band 0xff)
            }
            $stream.Write($buffer, 0, $count)
            $remaining -= $count
        }
    }
    finally { $stream.Dispose() }
}

Set-Content -LiteralPath (Join-Path $inputRoot "game.exe") -Value "synthetic benchmark executable" -NoNewline -Encoding UTF8
$dataPath = Join-Path $inputRoot "data.bin"
New-DeterministicFile $dataPath ([int64]$SizeMiB * 1024 * 1024) 424242

$cargoManifest = Join-Path $repoRoot "server\Cargo.toml"
$throughputPath = Join-Path $repoRoot "server\target\release\throughput.exe"
if (-not (Test-Path -LiteralPath $throughputPath)) {
    & cargo build --manifest-path $cargoManifest --release -p launcher-packager --bin throughput
    if ($LASTEXITCODE -ne 0) { throw "throughput benchmark build failed" }
}
$compressedPath = Join-Path $throughputRoot "data.zst"
$decompressedPath = Join-Path $throughputRoot "data.roundtrip.bin"
$throughputOutput = & $throughputPath $dataPath $compressedPath $decompressedPath
if ($LASTEXITCODE -ne 0) { throw "throughput benchmark failed" }
$throughput = ($throughputOutput -join [Environment]::NewLine) | ConvertFrom-Json

$adminPath = Join-Path $repoRoot "server\target\debug\launcher-admin.exe"
if (-not (Test-Path -LiteralPath $adminPath)) {
    & cargo build --manifest-path $cargoManifest --workspace
    if ($LASTEXITCODE -ne 0) { throw "admin build failed" }
}
$env:PYTHONPATH = Join-Path $repoRoot "analyzer\src"
$stdoutPath = Join-Path $outputRoot "packager.stdout.log"
$stderrPath = Join-Path $outputRoot "packager.stderr.log"
$arguments = @(
    "ingest", $inputRoot, "--output", $packageRoot,
    "--game-id", "performance-game", "--build-id", "performance-build",
    "--display-version", "baseline", "--executable", "game.exe",
    "--minimum-bytes", "65536", "--average-bytes", "262144", "--maximum-bytes", "1048576"
)
$packagerTimer = [Diagnostics.Stopwatch]::StartNew()
$packager = Start-Process -FilePath $adminPath -ArgumentList $arguments -WorkingDirectory $repoRoot -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -PassThru -WindowStyle Hidden
$peakWorkingSet = 0L
while (-not $packager.HasExited) {
    try {
        $packager.Refresh()
        $peakWorkingSet = [Math]::Max($peakWorkingSet, [int64]$packager.WorkingSet64)
    }
    catch [InvalidOperationException] { }
    Start-Sleep -Milliseconds 50
}
$packager.WaitForExit()
$packager.Refresh()
$packagerTimer.Stop()
$packagerExitCode = [int]$packager.ExitCode
if ($packagerExitCode -ne 0) { throw "packager benchmark failed with exit code $packagerExitCode; see $stderrPath" }
$report = Get-Content -Raw -LiteralPath (Join-Path $packageRoot "report.json") | ConvertFrom-Json

$result = [pscustomobject]@{
    size_mib = $SizeMiB
    throughput = $throughput
    packager_elapsed_ms = [Math]::Round($packagerTimer.Elapsed.TotalMilliseconds, 3)
    packager_peak_admin_working_set_bytes = $peakWorkingSet
    packager_raw_bytes = [int64]$report.raw_bytes
    packager_encoded_bytes = [int64]$report.encoded_bytes
    packager_chunks = [int64]$report.chunks
}
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $outputRoot "metrics.json") -Encoding UTF8
$result | ConvertTo-Json -Depth 8

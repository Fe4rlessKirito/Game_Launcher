param([Int64]$SizeBytes = 4294967296)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path $PSScriptRoot -Parent
$root = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts\performance-baseline-large"))
$artifacts = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts")) + [IO.Path]::DirectorySeparatorChar
if (-not $root.StartsWith($artifacts, [StringComparison]::OrdinalIgnoreCase)) { throw "Large benchmark output escaped artifacts" }
New-Item -ItemType Directory -Path $root | Out-Null
$inputRoot = Join-Path $root "input"
$outputRoot = Join-Path $root "package"
New-Item -ItemType Directory -Path $inputRoot | Out-Null
Set-Content -LiteralPath (Join-Path $inputRoot "game.exe") -Value "synthetic benchmark executable" -NoNewline -Encoding UTF8
$largePath = Join-Path $inputRoot "zero-filled.bin"
if (-not (Test-Path -LiteralPath $largePath) -or (Get-Item -LiteralPath $largePath).Length -ne $SizeBytes) {
    & fsutil file createnew $largePath $SizeBytes | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "could not create the deterministic zero-filled fixture" }
}
$env:PYTHONPATH = Join-Path $repoRoot "analyzer\src"
$admin = Join-Path $repoRoot "server\target\debug\launcher-admin.exe"
$stdout = Join-Path $root "packager.stdout.log"
$stderr = Join-Path $root "packager.stderr.log"
$arguments = @(
    "ingest", $inputRoot, "--output", $outputRoot,
    "--game-id", "large-streaming-game", "--build-id", "large-streaming-build",
    "--display-version", "baseline", "--executable", "game.exe",
    "--minimum-bytes", "65536", "--average-bytes", "262144", "--maximum-bytes", "1048576"
)
$timer = [Diagnostics.Stopwatch]::StartNew()
$process = Start-Process -FilePath $admin -ArgumentList $arguments -WorkingDirectory $repoRoot -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -WindowStyle Hidden
$peakWorkingSet = 0L
while (-not $process.HasExited) {
    try { $process.Refresh(); $peakWorkingSet = [Math]::Max($peakWorkingSet, [int64]$process.WorkingSet64) } catch [InvalidOperationException] { }
    Start-Sleep -Milliseconds 100
}
$process.WaitForExit()
$process.Refresh()
$timer.Stop()
$exitCode = [int]$process.ExitCode
if ($exitCode -ne 0) { throw "large packager failed with exit code $exitCode; see $stderr" }
$report = Get-Content -Raw -LiteralPath (Join-Path $outputRoot "report.json") | ConvertFrom-Json
$result = [pscustomobject]@{
    input_bytes = $SizeBytes
    elapsed_ms = [Math]::Round($timer.Elapsed.TotalMilliseconds, 3)
    peak_admin_working_set_bytes = $peakWorkingSet
    raw_bytes = [int64]$report.raw_bytes
    encoded_bytes = [int64]$report.encoded_bytes
    chunks = [int64]$report.chunks
    unique_chunks = [int64]$report.unique_chunks
}
$result | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root "metrics.json") -Encoding UTF8
$result | ConvertTo-Json

param(
    [string]$FixtureRoot = "artifacts\staging-fixture",
    [string]$PackageRoot = "artifacts\staging-packages",
    [string]$PrivateKeyPath = "artifacts\staging-keys\staging-2026-01.private.pem",
    [string]$WorkerService = "launcher-restore-worker",
    [string]$BuildAId = "staging-a",
    [string]$BuildBId = "staging-b",
    [switch]$KeepRemotePackages
)

. (Join-Path $PSScriptRoot "common.ps1")

if (-not (Get-Command railway -ErrorAction SilentlyContinue)) {
    throw "Railway CLI is required to upload packages and run the private worker publish command"
}
$fixture = Resolve-StagingPath $FixtureRoot
$packageDirectory = Assert-ArtifactPath (Resolve-StagingPath $PackageRoot)
$privateKey = [IO.Path]::GetFullPath($PrivateKeyPath)
if (-not (Test-Path -LiteralPath $fixture -PathType Container)) { throw "Fixture root does not exist: $fixture" }
if (-not (Test-Path -LiteralPath (Join-Path $fixture "A") -PathType Container)) { throw "Fixture A does not exist" }
if (-not (Test-Path -LiteralPath (Join-Path $fixture "B") -PathType Container)) { throw "Fixture B does not exist" }
if (-not (Test-Path -LiteralPath $privateKey -PathType Leaf) -and [string]::IsNullOrWhiteSpace($env:LAUNCHER_SIGNING_PRIVATE_KEY_PEM)) {
    throw "Provide the local staging private key path or set LAUNCHER_SIGNING_PRIVATE_KEY_PEM in this process"
}
if (Test-Path -LiteralPath $packageDirectory) { Remove-Item -LiteralPath $packageDirectory -Recurse -Force }
New-Item -ItemType Directory -Path $packageDirectory | Out-Null

$commonPackagerArgs = @(
    "--minimum-bytes", "65536",
    "--average-bytes", "262144",
    "--maximum-bytes", "1048576"
)
$packageA = Join-Path $packageDirectory "A"
$packageB = Join-Path $packageDirectory "B"
Invoke-LauncherAdmin (@(
    "ingest", (Join-Path $fixture "A"),
    "--output", $packageA,
    "--game-id", "synthetic-game",
    "--build-id", $BuildAId,
    "--display-version", "A",
    "--executable", "SyntheticGame.exe"
) + $commonPackagerArgs)
Invoke-LauncherAdmin (@(
    "ingest", (Join-Path $fixture "B"),
    "--output", $packageB,
    "--game-id", "synthetic-game",
    "--build-id", $BuildBId,
    "--display-version", "B",
    "--executable", "SyntheticGame.exe"
) + $commonPackagerArgs)

function Sign-Package([string]$Package, [string]$KeyPath) {
    $arguments = @(
        "manifest-sign",
        (Join-Path $Package "manifest.json"),
        "--output", (Join-Path $Package "manifest.sig.json"),
        "--key-id", "staging-2026-01"
    )
    if (Test-Path -LiteralPath $KeyPath -PathType Leaf) {
        $arguments += @("--private-key", $KeyPath)
    }
    Invoke-LauncherAdmin $arguments
}
Sign-Package $packageA $privateKey
Sign-Package $packageB $privateKey

$remoteRoot = "/var/lib/launcher/staging-publish"
$remoteA = "$remoteRoot/A"
$remoteB = "$remoteRoot/B"

# Railway CLI 5.x resolves service-scoped filesystem commands from the
# currently linked service instead of accepting --service on this subcommand.
Invoke-Checked -File "railway" -Arguments @("service", "link", $WorkerService)
Invoke-Checked -File "railway" -Arguments @(
    "service", "files", "upload",
    $packageA, $remoteA,
    "--overwrite"
)
Invoke-Checked -File "railway" -Arguments @(
    "service", "files", "upload",
    $packageB, $remoteB,
    "--overwrite"
)

$publishCommand = @(
    "set -eu",
    "chown -R launcher:launcher '$remoteRoot'",
    "export HOME=/var/lib/launcher/megacmd",
    "gosu launcher /usr/local/bin/launcher-admin publish '$remoteA' --catalog-root /var/lib/launcher/staging-catalog --storage-root /var/lib/launcher/storage",
    "gosu launcher /usr/local/bin/launcher-admin publish '$remoteB' --catalog-root /var/lib/launcher/staging-catalog --storage-root /var/lib/launcher/storage"
)
if (-not $KeepRemotePackages) {
    $publishCommand += "rm -rf '$remoteRoot'"
}
$remoteScript = $publishCommand -join "; "
Invoke-Checked -File "railway" -Arguments @(
    "ssh", "--service", $WorkerService,
    "--", "sh", "-lc", $remoteScript
)

Write-Output "synthetic_publish=PASS"
Write-Output "build_a_id=$BuildAId"
Write-Output "build_b_id=$BuildBId"
Write-Output "remote_packages=$remoteRoot"

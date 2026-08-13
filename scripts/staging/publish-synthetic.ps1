param(
    [string]$FixtureRoot = "artifacts\staging-fixture",
    [string]$PackageRoot = "artifacts\staging-packages",
    [string]$PrivateKeyPath = "artifacts\staging-keys\staging-2026-01.private.pem",
    [string]$WorkerService = "worker",
    [string]$BuildAId = "staging-a",
    [string]$BuildBId = "staging-b",
    [switch]$KeepRemotePackages,
    [switch]$Mantle,
    [switch]$Local,
    [string]$RemoteHost,
    [string]$RemoteUser = "debian",
    [string]$IdentityFile,
    [string]$RemoteDirectory = "/home/debian/vaultnode"
)

. (Join-Path $PSScriptRoot "common.ps1")

if ($Mantle -and $Local) { throw "Choose -Mantle or -Local, not both" }
if (-not $Mantle -and -not $Local) { $Local = $true }
if ($Mantle) {
    if ([string]::IsNullOrWhiteSpace($RemoteHost)) { throw "-RemoteHost is required with -Mantle" }
    if ([string]::IsNullOrWhiteSpace($IdentityFile)) { throw "-IdentityFile is required with -Mantle" }
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

if ($Local) {
    $catalogRoot = Resolve-StagingPath "artifacts\staging-catalog"
    $storageRoot = Resolve-StagingPath "artifacts\staging-storage"
    New-Item -ItemType Directory -Force -Path $catalogRoot, $storageRoot | Out-Null
    foreach ($package in @($packageA, $packageB)) {
        Invoke-LauncherAdmin @(
            "publish", $package,
            "--catalog-root", $catalogRoot,
            "--storage-root", $storageRoot
        )
    }
    $remoteRoot = $null
}
else {
    $remoteHostRoot = "$RemoteDirectory/.staging/staging-publish"
    $remoteContainerRoot = "/var/lib/launcher/staging-publish"
    $remoteA = "$remoteHostRoot/A"
    $remoteB = "$remoteHostRoot/B"

    Invoke-MantleShell -RemoteHost $RemoteHost -RemoteUser $RemoteUser `
        -IdentityFile $IdentityFile -Command "mkdir -p '$remoteHostRoot'"
    Copy-MantleDirectory -LocalPath $packageA -RemoteHost $RemoteHost `
        -RemoteUser $RemoteUser -IdentityFile $IdentityFile -RemotePath $remoteA
    Copy-MantleDirectory -LocalPath $packageB -RemoteHost $RemoteHost `
        -RemoteUser $RemoteUser -IdentityFile $IdentityFile -RemotePath $remoteB

    $remoteCommands = @(
        "set -eu",
        "cd '$RemoteDirectory'",
        "docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml cp '$remoteA' '$WorkerService`:$remoteContainerRoot/A'",
        "docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml cp '$remoteB' '$WorkerService`:$remoteContainerRoot/B'",
        "docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T '$WorkerService' chown -R launcher:launcher '$remoteContainerRoot'",
        "docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T '$WorkerService' gosu launcher /usr/local/bin/launcher-admin publish '$remoteContainerRoot/A' --catalog-root /var/lib/launcher/staging-catalog --storage-root /var/lib/launcher/storage",
        "docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T '$WorkerService' gosu launcher /usr/local/bin/launcher-admin publish '$remoteContainerRoot/B' --catalog-root /var/lib/launcher/staging-catalog --storage-root /var/lib/launcher/storage"
    )
    if (-not $KeepRemotePackages) {
        $remoteCommands += "rm -rf '$remoteHostRoot'"
        $remoteCommands += "docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T '$WorkerService' rm -rf '$remoteContainerRoot'"
    }
    Invoke-MantleShell -RemoteHost $RemoteHost -RemoteUser $RemoteUser `
        -IdentityFile $IdentityFile -Command ($remoteCommands -join "; ")
    $remoteRoot = $remoteContainerRoot
}

Write-Output "synthetic_publish=PASS"
Write-Output "build_a_id=$BuildAId"
Write-Output "build_b_id=$BuildBId"
Write-Output "remote_packages=$remoteRoot"

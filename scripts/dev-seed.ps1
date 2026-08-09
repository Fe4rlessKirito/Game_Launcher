param(
    [string]$OutputRoot = (Join-Path (Split-Path -Parent $PSScriptRoot) 'artifacts\synthetic')
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$buildRoot = Join-Path $OutputRoot 'source'
$packageRoot = Join-Path $OutputRoot 'package'
New-Item -ItemType Directory -Force -Path (Join-Path $buildRoot 'Game\Binaries') | Out-Null
Set-Content -LiteralPath (Join-Path $buildRoot 'Game\Binaries\SyntheticGame.exe') -Value 'authorized synthetic executable fixture' -NoNewline
Set-Content -LiteralPath (Join-Path $buildRoot 'Game\README.txt') -Value 'Synthetic Game A. This file is intentionally small.' -NoNewline

Push-Location $repoRoot
try {
    python -m launcher_analyzer analyze $buildRoot --output (Join-Path $OutputRoot 'analysis.json') --json
    cargo run --manifest-path server/Cargo.toml -p launcher-packager -- package $buildRoot --output $packageRoot --game-id synthetic-game --build-id synthetic-build-a --display-version 1.0.0 --executable Game/Binaries/SyntheticGame.exe
    Write-Output "Synthetic artifacts written to $OutputRoot"
    Write-Output "Run the API with: `$env:LAUNCHER_MANIFEST_PATH='$packageRoot\manifest.json'; `$env:LAUNCHER_STORAGE_ROOT='$packageRoot'; cargo run --manifest-path server/Cargo.toml -p launcher-api"
}
finally { Pop-Location }

[CmdletBinding()]
param(
    [string]$RarArchive,
    [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot),
    [switch]$KeepArtifacts
)

$ErrorActionPreference = 'Stop'
$repoRootFull = [IO.Path]::GetFullPath($RepoRoot)
$artifactRoot = [IO.Path]::GetFullPath((Join-Path $repoRootFull 'artifacts\archive-validation'))
$releaseAdmin = Join-Path $repoRootFull 'server\target\release\launcher-admin.exe'
$debugAdmin = Join-Path $repoRootFull 'server\target\debug\launcher-admin.exe'
$admin = if (Test-Path -LiteralPath $releaseAdmin) { $releaseAdmin } elseif (Test-Path -LiteralPath $debugAdmin) { $debugAdmin } else { throw 'Build launcher-admin first with cargo build --manifest-path server/Cargo.toml -p launcher-worker.' }

if (-not $KeepArtifacts -and (Test-Path -LiteralPath $artifactRoot)) {
    Remove-Item -LiteralPath $artifactRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $artifactRoot -Force | Out-Null
$fixtureRoot = Join-Path $artifactRoot 'fixture'
New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'bin') -Force | Out-Null
Set-Content -LiteralPath (Join-Path $fixtureRoot 'Game.exe') -Value 'authorized archive normalization fixture' -NoNewline
Set-Content -LiteralPath (Join-Path $fixtureRoot 'bin\config.txt') -Value 'portable config' -NoNewline

$archives = [ordered]@{}
$zip = Join-Path $artifactRoot 'fixture.zip'
Compress-Archive -Path (Join-Path $fixtureRoot '*') -DestinationPath $zip -Force
$archives['zip'] = $zip

$tar = Join-Path $artifactRoot 'fixture.tar'
tar -cf $tar -C $fixtureRoot .
$archives['tar'] = $tar

$sevenZip = Get-Command 7z,7zz -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $sevenZip -and (Test-Path -LiteralPath 'C:\Program Files\7-Zip\7z.exe')) {
    $sevenZip = Get-Item -LiteralPath 'C:\Program Files\7-Zip\7z.exe'
}
if (-not $sevenZip) {
    throw 'A 7-Zip executable is required for the 7z normalization probe.'
}
$seven = Join-Path $artifactRoot 'fixture.7z'
$sevenZipPath = if ($sevenZip -is [IO.FileInfo]) { $sevenZip.FullName } else { $sevenZip.Source }
& $sevenZipPath a '-bd' '-y' $seven (Join-Path $fixtureRoot '*') | Out-Null
if ($LASTEXITCODE -ne 0) { throw "7z fixture creation failed with exit code $LASTEXITCODE." }
$archives['7z'] = $seven

if ([string]::IsNullOrWhiteSpace($RarArchive)) {
    $downloadsRoot = Split-Path (Split-Path $repoRootFull -Parent) -Parent
    $knownRar = Join-Path $downloadsRoot 'Launcher Versions\Launcher 1.0.0.rar'
    if (Test-Path -LiteralPath $knownRar) { $RarArchive = $knownRar }
}
if ([string]::IsNullOrWhiteSpace($RarArchive) -or -not (Test-Path -LiteralPath $RarArchive -PathType Leaf)) {
    throw 'Pass -RarArchive with an authorized RAR fixture; RAR creation is not available in the Windows base toolchain.'
}
$archives['rar'] = [IO.Path]::GetFullPath($RarArchive)

$oldPackStorage = $env:PACK_STORAGE_ENABLED
$env:PACK_STORAGE_ENABLED = 'false'
try {
    foreach ($entry in $archives.GetEnumerator()) {
        $output = Join-Path $artifactRoot ("output-" + $entry.Key)
        New-Item -ItemType Directory -Path $output -Force | Out-Null
        $arguments = @(
            'ingest', $entry.Value,
            '--output', $output,
            '--game-id', 'archive-normalization-fixture',
            '--build-id', ('archive-' + $entry.Key),
            '--display-version', '1.0.0'
        )
        $outputText = & $admin @arguments 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0 -or $outputText -notmatch ('stage=NORMALIZED format=' + [regex]::Escape($entry.Key))) {
            throw "Archive normalization failed for $($entry.Key):`n$outputText"
        }
        if ($outputText -notmatch 'stage=Ready') {
            throw "Archive normalization did not reach READY for $($entry.Key):`n$outputText"
        }
        Write-Output ("archive={0} status=PASS" -f $entry.Key)
    }
}
finally {
    if ($null -eq $oldPackStorage) { Remove-Item Env:PACK_STORAGE_ENABLED -ErrorAction SilentlyContinue }
    else { $env:PACK_STORAGE_ENABLED = $oldPackStorage }
    if (-not $KeepArtifacts -and (Test-Path -LiteralPath $artifactRoot)) {
        Remove-Item -LiteralPath $artifactRoot -Recurse -Force
    }
}

Write-Output 'archive_normalization=PASS formats=zip,tar,7z,rar'

param(
    [string]$OutputRoot = (Join-Path (Split-Path $PSScriptRoot -Parent) "artifacts\e2e\synthetic-game")
)

$repoRoot = Split-Path $PSScriptRoot -Parent
$outputRoot = [IO.Path]::GetFullPath($OutputRoot)
$expectedPrefix = [IO.Path]::GetFullPath((Join-Path $repoRoot "artifacts")) + [IO.Path]::DirectorySeparatorChar
if (-not $outputRoot.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Synthetic fixture output must remain under $expectedPrefix" }
if (Test-Path -LiteralPath $outputRoot) { Remove-Item -LiteralPath $outputRoot -Recurse -Force }
$tempRoot = Join-Path $outputRoot "build-output"
$aRoot = Join-Path $outputRoot "A"
$bRoot = Join-Path $outputRoot "B"
New-Item -ItemType Directory -Path $aRoot, $bRoot, $tempRoot | Out-Null

$dotnet = Join-Path $repoRoot ".dotnet\dotnet.exe"
if (-not (Test-Path -LiteralPath $dotnet)) { $dotnet = "dotnet" }
$project = Join-Path $repoRoot "tests\SyntheticGame\SyntheticGame.csproj"

function Build-Version([string]$Version, [string]$Destination) {
    $buildOutput = Join-Path $tempRoot $Version
    & $dotnet build $project --configuration Release --output $buildOutput "/p:DefineConstants=VERSION_$Version" --nologo
    if ($LASTEXITCODE -ne 0) { throw "SyntheticGame build $Version failed" }
    Get-ChildItem -LiteralPath $buildOutput | Copy-Item -Destination $Destination -Recurse -Force
}

function New-DeterministicFile([string]$Path, [int64]$Length, [uint32]$Seed) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $buffer = New-Object byte[] 65536
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

function New-InsertedFile([string]$Source, [string]$Destination) {
    $input = [IO.File]::OpenRead($Source)
    $output = [IO.File]::Open($Destination, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $prefix = New-Object byte[] 32768
        $read = $input.Read($prefix, 0, $prefix.Length)
        $output.Write($prefix, 0, $read)
        $output.Write((New-Object byte[] 137), 0, 137)
        $input.CopyTo($output)
    }
    finally { $input.Dispose(); $output.Dispose() }
}

Build-Version "A" $aRoot
Build-Version "B" $bRoot
foreach ($root in @($aRoot, $bRoot)) { New-Item -ItemType Directory -Path (Join-Path $root "Data") | Out-Null }
Set-Content -LiteralPath (Join-Path $aRoot "Data\shared.txt") -Value "stable data" -NoNewline -Encoding UTF8
Set-Content -LiteralPath (Join-Path $bRoot "Data\shared.txt") -Value "stable data" -NoNewline -Encoding UTF8
New-DeterministicFile (Join-Path $aRoot "Data\shared-large.bin") 4194304 12345
Copy-Item -LiteralPath (Join-Path $aRoot "Data\shared-large.bin") -Destination (Join-Path $bRoot "Data\shared-large.bin")
New-DeterministicFile (Join-Path $aRoot "Data\inserted.bin") 4194304 67890
New-InsertedFile (Join-Path $aRoot "Data\inserted.bin") (Join-Path $bRoot "Data\inserted.bin")
Set-Content -LiteralPath (Join-Path $aRoot "Data\changed.txt") -Value "version A" -NoNewline -Encoding UTF8
Set-Content -LiteralPath (Join-Path $bRoot "Data\changed.txt") -Value "version B with a concrete change" -NoNewline -Encoding UTF8
Set-Content -LiteralPath (Join-Path $aRoot "Data\removed.txt") -Value "removed in version B" -NoNewline -Encoding UTF8
New-DeterministicFile (Join-Path $bRoot "Data\added.bin") 1048576 24680

[pscustomobject]@{ version_a = $aRoot; version_b = $bRoot } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $outputRoot "fixture.json") -Encoding UTF8
Write-Output (Join-Path $outputRoot "fixture.json")

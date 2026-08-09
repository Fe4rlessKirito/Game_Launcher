param([Parameter(Mandatory = $true)][string]$ManifestPath)
$ErrorActionPreference = 'Stop'
$manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
if ($manifest.schema_version -ne 1) { throw "Unsupported schema version" }
if ($manifest.chunking.algorithm -ne 'fastcdc') { throw "Unsupported chunking algorithm" }
if ($manifest.encoding.id -ne 'zstd-v1-level-3') { throw "Unsupported encoding" }
$paths = @{}
foreach ($file in $manifest.files) {
    if ($file.path.Contains('\') -or $file.path.StartsWith('/') -or $file.path.Contains('..')) { throw "Unsafe path: $($file.path)" }
    if ($paths.ContainsKey($file.path)) { throw "Duplicate path: $($file.path)" }
    $paths[$file.path] = $true
}
if (-not $paths.ContainsKey($manifest.launch.executable)) { throw "Launch executable is not owned by manifest" }
Write-Output "Manifest valid: $ManifestPath"

param(
    [Parameter(Mandatory = $true)][string]$ApiUrl,
    [Parameter(Mandatory = $true)][string]$BuildId,
    [int]$PartialBytes = 1048576,
    [string]$OutputPath = "artifacts\staging-range\pack-reconstructed.bin"
)

. (Join-Path $PSScriptRoot "common.ps1")
Add-Type -AssemblyName System.Net.Http

$output = Assert-ArtifactPath (Resolve-StagingPath $OutputPath)
$baseUri = [Uri]($ApiUrl.TrimEnd("/") + "/")
$escapedBuild = [Uri]::EscapeDataString($BuildId)
$manifest = Invoke-RestMethod -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/manifest"))
$chunk = @($manifest.files | ForEach-Object { $_.chunks } | Select-Object -First 1)
if ($null -eq $chunk -or $chunk.Count -eq 0) { throw "Build manifest contains no chunks" }
$body = @{ encoded_hashes = @($chunk[0].encoded_hash) } | ConvertTo-Json -Compress
$resolved = Invoke-RestMethod -Method Post -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/packs/resolve")) -ContentType "application/json" -Body $body
$pack = @($resolved)[0]
$source = @($pack.sources | Where-Object { $_.range_supported -and $_.url }) | Select-Object -First 1
if ($null -eq $source) { throw "No direct pack source with observed range support was returned" }
$encodedSize = [int64]$pack.encoded_size
if ($encodedSize -lt 2) { throw "Selected physical pack is too small for a range test" }
$partialLength = [Math]::Min([Math]::Max(1, $PartialBytes), [int]($encodedSize - 1))

$client = [Net.Http.HttpClient]::new()
try {
    New-Item -ItemType Directory -Force -Path (Split-Path $output -Parent) | Out-Null
    $firstRequest = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, [Uri]$source.url)
    $firstRequest.Headers.Range = [Net.Http.Headers.RangeHeaderValue]::new(0, $partialLength - 1)
    $firstResponse = $client.SendAsync($firstRequest).GetAwaiter().GetResult()
    if ([int]$firstResponse.StatusCode -ne 206) { throw "Initial pack range returned HTTP $([int]$firstResponse.StatusCode), not 206" }
    $firstStream = $firstResponse.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
    try {
        $outputStream = [IO.File]::Create($output)
        try { $firstStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
    } finally { $firstStream.Dispose(); $firstResponse.Dispose() }

    $resumeRequest = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, [Uri]$source.url)
    $resumeRequest.Headers.Range = [Net.Http.Headers.RangeHeaderValue]::new($partialLength, $encodedSize - 1)
    $resumeResponse = $client.SendAsync($resumeRequest).GetAwaiter().GetResult()
    if ([int]$resumeResponse.StatusCode -ne 206) { throw "Resume pack range returned HTTP $([int]$resumeResponse.StatusCode), not 206" }
    $resumeStream = $resumeResponse.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
    try {
        $outputStream = [IO.File]::Open($output, [IO.FileMode]::Append, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        try { $resumeStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
    } finally { $resumeStream.Dispose(); $resumeResponse.Dispose() }
}
finally { $client.Dispose() }

$finalLength = (Get-Item -LiteralPath $output).Length
$finalHash = Get-LauncherBlake3 $output
if ($finalLength -ne $encodedSize) { throw "Reconstructed pack length $finalLength does not equal $encodedSize" }
if ($finalHash -ne $pack.pack_hash) { throw "Reconstructed pack BLAKE3 $finalHash does not equal $($pack.pack_hash)" }
Write-Output "physical_pack_range_resume=PASS"
Write-Output "provider=$($source.provider)"
Write-Output "initial_status=206"
Write-Output "resume_status=206"
Write-Output "partial_file_size=$partialLength"
Write-Output "final_bytes_received=$finalLength"
Write-Output "final_hash=$finalHash"

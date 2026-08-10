param(
    [Parameter(Mandatory = $true)][string]$ApiUrl,
    [Parameter(Mandatory = $true)][string]$BuildId,
    [string]$EncodedHash,
    [int]$PartialBytes = 65536,
    [string]$OutputPath = "artifacts\staging-range\reconstructed.bin"
)

. (Join-Path $PSScriptRoot "common.ps1")

$output = Assert-ArtifactPath (Resolve-StagingPath $OutputPath)
$baseUri = [Uri]($ApiUrl.TrimEnd("/") + "/")
$escapedBuild = [Uri]::EscapeDataString($BuildId)
$manifest = Invoke-RestMethod -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/manifest"))
$chunks = @($manifest.files | ForEach-Object { $_.chunks })
if ($chunks.Count -eq 0) { throw "Build manifest contains no chunks" }
if ([string]::IsNullOrWhiteSpace($EncodedHash)) {
    $chunk = $chunks[0]
}
else {
    $chunk = $chunks | Where-Object { $_.encoded_hash -eq $EncodedHash } | Select-Object -First 1
    if ($null -eq $chunk) { throw "EncodedHash is not present in the selected manifest" }
}
$encodedSize = [int64]$chunk.encoded_size
$partialLength = [Math]::Min([Math]::Max(1, $PartialBytes), [int]($encodedSize - 1))
if ($encodedSize -lt 2) { throw "Selected chunk is too small for a range-resume test" }
$body = @{ encoded_hashes = @($chunk.encoded_hash) } | ConvertTo-Json -Compress
$resolved = Invoke-RestMethod -Method Post -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/resolve")) -ContentType "application/json" -Body $body
$url = [string](@($resolved)[0].urls[0])

$client = [Net.Http.HttpClient]::new()
try {
    $firstRequest = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, $url)
    $firstRequest.Headers.Range = [Net.Http.Headers.RangeHeaderValue]::new(0, $partialLength - 1)
    $firstResponse = $client.SendAsync($firstRequest).GetAwaiter().GetResult()
    $firstBytes = $firstResponse.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
    if ([int]$firstResponse.StatusCode -ne 206) {
        throw "Initial range request returned HTTP $([int]$firstResponse.StatusCode), not 206"
    }
    if ($firstBytes.Length -ne $partialLength) {
        throw "Initial range returned $($firstBytes.Length) bytes; expected $partialLength"
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $output -Parent) | Out-Null
    [IO.File]::WriteAllBytes($output, $firstBytes)

    $resumeRequest = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, $url)
    $resumeRequest.Headers.Range = [Net.Http.Headers.RangeHeaderValue]::new($partialLength, $encodedSize - 1)
    $resumeResponse = $client.SendAsync($resumeRequest).GetAwaiter().GetResult()
    $resumeBytes = $resumeResponse.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
    $resumeStatus = [int]$resumeResponse.StatusCode
    if ($resumeStatus -ne 206) {
        throw "Resume range request returned HTTP $resumeStatus, not 206"
    }
    if ($resumeBytes.Length -ne ($encodedSize - $partialLength)) {
        throw "Resume range returned $($resumeBytes.Length) bytes; expected $($encodedSize - $partialLength)"
    }
    $stream = [IO.File]::Open($output, [IO.FileMode]::Append, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    try { $stream.Write($resumeBytes, 0, $resumeBytes.Length) } finally { $stream.Dispose() }
}
finally {
    $client.Dispose()
}

$finalLength = (Get-Item -LiteralPath $output).Length
$finalHash = Get-LauncherBlake3 $output
if ($finalLength -ne $encodedSize) { throw "Reconstructed file length $finalLength does not equal manifest size $encodedSize" }
if ($finalHash -ne $chunk.encoded_hash) { throw "Reconstructed BLAKE3 $finalHash does not equal $($chunk.encoded_hash)" }
Write-Output "range_resume=PASS"
Write-Output "initial_bytes_received=$($firstBytes.Length)"
Write-Output "partial_file_size=$partialLength"
Write-Output "initial_status=206"
Write-Output "resume_bytes_received=$($resumeBytes.Length)"
Write-Output "resume_status=206"
Write-Output "final_bytes_received=$finalLength"
Write-Output "final_hash=$finalHash"

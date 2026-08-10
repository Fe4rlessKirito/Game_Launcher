param(
    [Parameter(Mandatory = $true)][string]$ApiUrl,
    [Parameter(Mandatory = $true)][string]$BuildId,
    [string]$EncodedHash,
    [int]$WaitSeconds = 4,
    [string]$OutputPath = "artifacts\staging-presign\downloaded.bin"
)

. (Join-Path $PSScriptRoot "common.ps1")

if ($WaitSeconds -lt 1) { throw "WaitSeconds must be positive" }
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
$body = @{ encoded_hashes = @($chunk.encoded_hash) } | ConvertTo-Json -Compress

function Resolve-ChunkUrl {
    $resolved = Invoke-RestMethod -Method Post -Uri ([Uri]::new($baseUri, "api/v1/builds/$escapedBuild/resolve")) -ContentType "application/json" -Body $body
    return [string](@($resolved)[0].urls[0])
}

$expiredUrl = Resolve-ChunkUrl
Write-Output "presign_initial=RESOLVED"
Start-Sleep -Seconds $WaitSeconds
$client = [Net.Http.HttpClient]::new()
try {
    $expiredResponse = $client.GetAsync($expiredUrl).GetAwaiter().GetResult()
    if ($expiredResponse.IsSuccessStatusCode) {
        throw "Presigned URL still succeeded after $WaitSeconds seconds; configure a shorter staging TTL and retry"
    }
    Write-Output "presign_expired=OBSERVED http=$([int]$expiredResponse.StatusCode)"
    $refreshedUrl = Resolve-ChunkUrl
    $refreshedResponse = $client.GetAsync($refreshedUrl).GetAwaiter().GetResult()
    $refreshedResponse.EnsureSuccessStatusCode()
    $bytes = $refreshedResponse.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
}
finally {
    $client.Dispose()
}

New-Item -ItemType Directory -Force -Path (Split-Path $output -Parent) | Out-Null
[IO.File]::WriteAllBytes($output, $bytes)
$hash = Get-LauncherBlake3 $output
if ($hash -ne $chunk.encoded_hash) { throw "Refreshed download BLAKE3 $hash does not equal $($chunk.encoded_hash)" }
Write-Output "presign_refresh=PASS"
Write-Output "refreshed_bytes=$($bytes.Length)"
Write-Output "refreshed_hash=$hash"

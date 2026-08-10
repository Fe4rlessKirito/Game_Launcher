param(
    [string]$KeyOutputDirectory = "artifacts\staging-keys",
    [string]$FixtureOutputRoot = "artifacts\staging-fixture",
    [string]$KeyId = "staging-2026-01",
    [switch]$SkipFixture,
    [switch]$ForceKey
)

. (Join-Path $PSScriptRoot "common.ps1")

$keyDirectory = Assert-ArtifactPath (Resolve-StagingPath $KeyOutputDirectory)
$fixtureRoot = Assert-ArtifactPath (Resolve-StagingPath $FixtureOutputRoot)
$keyArguments = @(
    "signing", "init-staging",
    "--output-dir", $keyDirectory,
    "--key-id", $KeyId
)
if ($ForceKey) { $keyArguments += "--force" }
Invoke-LauncherAdmin $keyArguments

if (-not $SkipFixture) {
    $generator = Join-Path $script:StagingRepoRoot "scripts\generate-synthetic-game.ps1"
    Invoke-Checked -File "powershell" -Arguments @(
        "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", $generator,
        "-OutputRoot", $fixtureRoot
    )
}

$publicKey = Join-Path $keyDirectory "$KeyId.public.pem"
$privateKey = Join-Path $keyDirectory "$KeyId.private.pem"
Write-Output "staging_setup=READY"
Write-Output "staging_key_id=$KeyId"
Write-Output "staging_public_key=$publicKey"
Write-Output "staging_private_key=$privateKey"
Write-Output "private_key_action=store directly in the Railway secret; never commit or send it"
if (-not $SkipFixture) {
    Write-Output "synthetic_fixture=$fixtureRoot"
}

$ErrorActionPreference = "Stop"

$script:StagingRepoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))

function Get-LauncherDotnet {
    $candidate = Join-Path $script:StagingRepoRoot ".dotnet\dotnet.exe"
    if (Test-Path -LiteralPath $candidate) { return $candidate }
    return "dotnet"
}

function Get-LauncherAdmin {
    $release = Join-Path $script:StagingRepoRoot "server\target\release\launcher-admin.exe"
    if (Test-Path -LiteralPath $release) { return $release }
    $debug = Join-Path $script:StagingRepoRoot "server\target\debug\launcher-admin.exe"
    if (Test-Path -LiteralPath $debug) { return $debug }
    return $null
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [string]$WorkingDirectory = $script:StagingRepoRoot
    )

    Push-Location $WorkingDirectory
    try {
        & $File @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "$File $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

function Invoke-LauncherAdmin {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)

    $admin = Get-LauncherAdmin
    if ($null -ne $admin) {
        Invoke-Checked -File $admin -Arguments $Arguments
        return
    }
    $cargoArguments = @(
        "run",
        "--manifest-path", (Join-Path $script:StagingRepoRoot "server\Cargo.toml"),
        "-p", "launcher-worker",
        "--bin", "launcher-admin",
        "--"
    ) + $Arguments
    Invoke-Checked -File "cargo" -Arguments $cargoArguments
}

function Get-LauncherBlake3 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $admin = Get-LauncherAdmin
    if ($null -ne $admin) {
        $lines = & $admin "hash-file" $Path
    }
    else {
        $cargoArguments = @(
            "run",
            "--manifest-path", (Join-Path $script:StagingRepoRoot "server\Cargo.toml"),
            "-p", "launcher-worker",
            "--bin", "launcher-admin",
            "--",
            "hash-file", $Path
        )
        $lines = & cargo @cargoArguments
    }
    if ($LASTEXITCODE -ne 0) {
        throw "Could not calculate BLAKE3 for $Path"
    }
    $line = $lines | Where-Object { $_ -match "^blake3=([0-9a-f]{64})\s" } | Select-Object -Last 1
    if ($null -eq $line) { throw "launcher-admin hash-file returned no BLAKE3 value" }
    return ([regex]::Match($line, "^blake3=([0-9a-f]{64})").Groups[1].Value)
}

function Resolve-StagingPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $script:StagingRepoRoot $Path))
}

function Assert-ArtifactPath {
    param([Parameter(Mandatory = $true)][string]$Path)
    $fullPath = [IO.Path]::GetFullPath($Path)
    $artifactRoot = [IO.Path]::GetFullPath((Join-Path $script:StagingRepoRoot "artifacts")) + [IO.Path]::DirectorySeparatorChar
    if (-not $fullPath.StartsWith($artifactRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Staging script output must remain under $artifactRoot"
    }
    return $fullPath
}

function Invoke-RailwayAdmin {
    param(
        [Parameter(Mandatory = $true)][string]$Service,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $commandLine = "export HOME=/var/lib/launcher/telegram; exec gosu launcher /usr/local/bin/launcher-admin " +
        (($Arguments | ForEach-Object { "'" + ($_ -replace "'", "'\''") + "'" }) -join " ")
    Invoke-Checked -File "railway" -Arguments @("ssh", "--service", $Service, "--", "sh", "-lc", $commandLine)
}

function Get-MantleIdentityFile {
    param([Parameter(Mandatory = $true)][string]$IdentityFile)

    $resolvedIdentity = [IO.Path]::GetFullPath($IdentityFile)
    if (-not (Test-Path -LiteralPath $resolvedIdentity -PathType Leaf)) {
        throw "SSH identity file was not found: $resolvedIdentity"
    }
    return $resolvedIdentity
}

function Invoke-MantleShell {
    param(
        [Parameter(Mandatory = $true)][string]$RemoteHost,
        [Parameter(Mandatory = $true)][string]$IdentityFile,
        [Parameter(Mandatory = $true)][string]$Command,
        [string]$RemoteUser = "debian"
    )

    $resolvedIdentity = Get-MantleIdentityFile $IdentityFile
    Invoke-Checked -File "ssh" -Arguments @(
        "-i", $resolvedIdentity,
        "-o", "BatchMode=yes",
        "$RemoteUser@$RemoteHost",
        $Command
    )
}

function Copy-MantleDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$LocalPath,
        [Parameter(Mandatory = $true)][string]$RemoteHost,
        [Parameter(Mandatory = $true)][string]$IdentityFile,
        [Parameter(Mandatory = $true)][string]$RemotePath,
        [string]$RemoteUser = "debian"
    )

    $resolvedIdentity = Get-MantleIdentityFile $IdentityFile
    $resolvedLocalPath = [IO.Path]::GetFullPath($LocalPath)
    if (-not (Test-Path -LiteralPath $resolvedLocalPath -PathType Container)) {
        throw "Local package directory was not found: $resolvedLocalPath"
    }
    Invoke-Checked -File "scp" -Arguments @(
        "-r",
        "-i", $resolvedIdentity,
        "-o", "BatchMode=yes",
        $resolvedLocalPath,
        "$RemoteUser@${RemoteHost}:$RemotePath"
    )
}

function Invoke-MantleAdmin {
    param(
        [Parameter(Mandatory = $true)][string]$RemoteHost,
        [Parameter(Mandatory = $true)][string]$IdentityFile,
        [string]$RemoteUser = "debian",
        [string]$RemoteDirectory = "/home/debian/vaultnode",
        [string]$Service = "worker",
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $quoteRemote = {
        param([string]$Value)
        return "'" + ($Value -replace "'", "'\\''") + "'"
    }
    $adminArguments = (($Arguments | ForEach-Object { & $quoteRemote ([string]$_) }) -join " ")
    $remoteCommand = "cd " + (& $quoteRemote $RemoteDirectory) +
        " && docker compose --env-file .env -f deploy/compose.yaml -f deploy/vps.compose.override.yaml exec -T " +
        (& $quoteRemote $Service) +
        " /usr/local/bin/launcher-admin " + $adminArguments

    Invoke-MantleShell -RemoteHost $RemoteHost -IdentityFile $IdentityFile `
        -RemoteUser $RemoteUser -Command $remoteCommand
}

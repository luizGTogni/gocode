[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArchivePath
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) {
    throw "Release archive not found: $ArchivePath"
}

$installRoot = Join-Path $env:USERPROFILE '.gocode'
$binDirectory = Join-Path $installRoot 'bin'
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("gocode-install-" + [guid]::NewGuid())

try {
    Expand-Archive -LiteralPath $ArchivePath -DestinationPath $staging -Force
    $payload = Get-Item -LiteralPath $staging
    foreach ($name in 'gocode.exe', 'gocode-updater.exe') {
        if (-not (Test-Path -LiteralPath (Join-Path $payload.FullName $name) -PathType Leaf)) {
            throw "Release archive is missing $name."
        }
    }
    New-Item -ItemType Directory -Force -Path $binDirectory | Out-Null
    Copy-Item (Join-Path $payload.FullName 'gocode.exe') (Join-Path $binDirectory 'gocode.exe') -Force
    Copy-Item (Join-Path $payload.FullName 'gocode-updater.exe') (Join-Path $binDirectory 'gocode-updater.exe') -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $segments = @($userPath -split ';' | Where-Object { $_ })
    if ($segments -notcontains $binDirectory) {
        [Environment]::SetEnvironmentVariable('Path', (($segments + $binDirectory) -join ';'), 'User')
    }
    Write-Host "Gocode installed. Open a new terminal and run: gocode"
}
finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $staging
}

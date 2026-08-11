param(
    [string]$Configuration = "release"
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$distRoot = Join-Path $repoRoot 'dist'
$staging = Join-Path $distRoot 'cruisemesh-helper-windows-x64'
$archive = Join-Path $distRoot 'cruisemesh-helper-windows-x64.zip'

cargo build --manifest-path (Join-Path $repoRoot 'Cargo.toml') --profile $Configuration -p cruisemesh-node
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
$uiRoot = Join-Path $PSScriptRoot 'ui'
Push-Location -LiteralPath $uiRoot
try {
    npm ci
    if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
    npm run tauri build -- --no-bundle
    if ($LASTEXITCODE -ne 0) { throw 'Tauri build failed' }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Path $distRoot -Force | Out-Null
$resolvedDist = (Resolve-Path -LiteralPath $distRoot).Path
$expectedStaging = [System.IO.Path]::GetFullPath($staging)
if (-not $expectedStaging.StartsWith($resolvedDist + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to stage outside the repository dist directory'
}
if (Test-Path -LiteralPath $staging) {
    $item = Get-Item -LiteralPath $staging -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
        throw 'Refusing to replace a non-directory or reparse-point staging path'
    }
    Remove-Item -LiteralPath $staging -Recurse -Force
}
New-Item -ItemType Directory -Path $staging | Out-Null

$binary = Join-Path $repoRoot "target\$Configuration\cruisemesh-node.exe"
Copy-Item -LiteralPath $binary -Destination $staging
$uiBinary = Join-Path $uiRoot "src-tauri\target\$Configuration\CruiseMesh.exe"
Copy-Item -LiteralPath $uiBinary -Destination $staging
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'DOGFOOD.md') -Destination (Join-Path $staging 'README.md')
if (Test-Path -LiteralPath $archive) {
    $archiveItem = Get-Item -LiteralPath $archive -Force
    if ($archiveItem.PSIsContainer -or ($archiveItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
        throw 'Refusing to replace a directory or reparse-point archive path'
    }
}
Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $archive -Force
Get-FileHash -Algorithm SHA256 -LiteralPath $archive

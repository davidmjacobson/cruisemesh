[CmdletBinding()]
param(
    [ValidateSet("ui", "all")]
    [string]$Suite = "ui",
    [string]$Ref,
    [switch]$Push,
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
Set-Location -LiteralPath $repoRoot

foreach ($command in @("git", "gh")) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Required command '$command' was not found on PATH."
    }
}

& gh auth status *> $null
if ($LASTEXITCODE -ne 0) {
    throw "GitHub CLI is not authenticated. Run 'gh auth login' first."
}

$dirty = & git status --porcelain
if ($LASTEXITCODE -ne 0) { throw "Could not inspect the Git worktree." }
if ($dirty) {
    throw "The worktree has uncommitted changes. Commit them so the remote Mac tests the exact code you expect."
}

$headSha = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or -not $headSha) { throw "Could not resolve HEAD." }

if (-not $Ref) {
    $Ref = (& git branch --show-current).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $Ref) {
        throw "HEAD is detached. Pass -Ref with the remote branch to test."
    }
}

if ($Push) {
    & git push origin "HEAD:refs/heads/$Ref"
    if ($LASTEXITCODE -ne 0) { throw "Could not push HEAD to origin/$Ref." }
}

$remoteLine = & git ls-remote --heads origin "refs/heads/$Ref"
if ($LASTEXITCODE -ne 0) { throw "Could not inspect origin/$Ref." }
$remoteSha = if ($remoteLine) { ($remoteLine -split "\s+")[0] } else { "" }
if ($remoteSha -ne $headSha) {
    throw "origin/$Ref is not at local HEAD $headSha. Push it first, or rerun with -Push."
}

$startedAt = [DateTimeOffset]::UtcNow
& gh workflow run ios.yml --ref $Ref -f "suite=$Suite"
if ($LASTEXITCODE -ne 0) { throw "Could not dispatch ios.yml for $Ref." }

$run = $null
for ($attempt = 0; $attempt -lt 30 -and -not $run; $attempt++) {
    Start-Sleep -Seconds 2
    $json = & gh run list `
        --workflow ios.yml `
        --branch $Ref `
        --event workflow_dispatch `
        --limit 20 `
        --json databaseId,headSha,createdAt,status,url
    if ($LASTEXITCODE -ne 0) { throw "Could not find the dispatched workflow run." }
    $matches = @($json | ConvertFrom-Json | Where-Object {
        $_.headSha -eq $headSha -and
        [DateTimeOffset]$_.createdAt -ge $startedAt.AddSeconds(-5)
    })
    if ($matches.Count -gt 1) {
        throw "More than one matching workflow was dispatched; refusing to watch the wrong run."
    }
    if ($matches.Count -eq 1) { $run = $matches[0] }
}

if (-not $run) { throw "The dispatched workflow did not appear within 60 seconds." }
Write-Host "Watching $($run.url) for commit $headSha"

& gh run watch $run.databaseId --exit-status
$testExitCode = $LASTEXITCODE

if (-not $OutputDirectory) {
    $shortSha = $headSha.Substring(0, 12)
    $OutputDirectory = Join-Path $repoRoot "tmp\ios-ui\$shortSha-$($run.databaseId)"
}
$resolvedParent = Split-Path -Parent $OutputDirectory
if ($resolvedParent) { New-Item -ItemType Directory -Force -Path $resolvedParent | Out-Null }

& gh run download $run.databaseId -n ios-test-results -D $OutputDirectory
$downloadExitCode = $LASTEXITCODE
if ($downloadExitCode -ne 0) {
    throw "The workflow finished, but its ios-test-results artifact could not be downloaded."
}

Write-Host "Downloaded .xcresult and build log to $OutputDirectory"
if ($testExitCode -ne 0) {
    throw "The iOS test workflow failed. Inspect the downloaded evidence above."
}

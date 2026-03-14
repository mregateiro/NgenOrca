param(
    [ValidateSet('docker', 'nas', 'cargo')]
    [string]$Mode = 'docker',

    [string]$Branch = 'master'
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

Write-Host "==> Updating repository ($Branch)"
git fetch origin --prune
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

git checkout $Branch
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

git pull --ff-only origin $Branch
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

switch ($Mode) {
    'docker' {
        Write-Host '==> Redeploying Docker stack'
        docker compose up -d --build
    }
    'nas' {
        Write-Host '==> Redeploying NAS Docker stack'
        docker compose -f docker-compose.nas.yml up -d --build
    }
    'cargo' {
        Write-Host '==> Reinstalling native CLI from current source'
        cargo install --path crates/ngenorca-cli --locked --force
    }
}

if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host '==> Done'

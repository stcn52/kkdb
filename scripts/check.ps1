param(
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$targetDir = Join-Path $repoRoot ("target\\ci_" + $PID)

$env:CARGO_TARGET_DIR = $targetDir
$env:CARGO_INCREMENTAL = "0"
$env:RUSTFLAGS = "-C codegen-units=1"

Write-Host "Using CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"

function Invoke-Step {
    param([string]$Command)
    Write-Host ">> $Command"
    Invoke-Expression $Command
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $Command"
    }
}

Invoke-Step "cargo fmt --all --check"
Invoke-Step "cargo clippy --all-targets --all-features -- -A clippy::approx_constant"

if ($Release) {
    Invoke-Step "cargo test --release -j 1"
} else {
    Invoke-Step "cargo test -j 1"
}

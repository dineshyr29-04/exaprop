# Run the repository's development checks from any working directory.
$ErrorActionPreference = 'Stop'

function Invoke-CargoCheck {
    param([string[]]$Arguments)

    Write-Host "cargo $($Arguments -join ' ')"
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo check failed with exit code ${LASTEXITCODE}: cargo $($Arguments -join ' ')"
    }
}

$projectDirectory = Split-Path -Parent $PSScriptRoot
$previousRustdocFlags = $env:RUSTDOCFLAGS
Push-Location -LiteralPath $projectDirectory
try {
    Invoke-CargoCheck -Arguments @('fmt', '--all', '--', '--check')
    Invoke-CargoCheck -Arguments @('check', '--workspace', '--locked')
    Invoke-CargoCheck -Arguments @('test', '--workspace', '--locked')
    Invoke-CargoCheck -Arguments @('clippy', '--workspace', '--all-targets', '--all-features', '--locked', '--', '-D', 'warnings')
    Invoke-CargoCheck -Arguments @('build', '--workspace', '--locked')
    Invoke-CargoCheck -Arguments @('build', '--workspace', '--release', '--locked')
    $env:RUSTDOCFLAGS = "$previousRustdocFlags -D warnings".Trim()
    Invoke-CargoCheck -Arguments @('doc', '--workspace', '--no-deps', '--locked')
    Write-Host 'All development checks passed.'
} finally {
    $env:RUSTDOCFLAGS = $previousRustdocFlags
    Pop-Location
}

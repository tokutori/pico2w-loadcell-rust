param(
    [string]$Port = 'COM5',
    [switch]$Demo
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$hostLine = rustc -vV | Select-String '^host: '
if ($LASTEXITCODE -ne 0 -or -not $hostLine) { throw 'rustc is required.' }
$hostTarget = $hostLine.ToString().Substring(6).Trim()
$manifest = Join-Path $PSScriptRoot 'Cargo.toml'
$appArguments = @('--port', $Port)
if ($Demo) { $appArguments += '--demo' }
# Explicit host target overrides the parent firmware's ARM target.
cargo run --locked --manifest-path $manifest --target $hostTarget -p serial-plotter -- @appArguments
exit $LASTEXITCODE

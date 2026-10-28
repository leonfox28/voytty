# Canonical Windows workspace checks, shared by just and both native CI runners.
param([string]$Transport = 'true')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Invoke-Checked([scriptblock]$Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "Command failed: $Command" }
}
if ($Transport -notin @('true', 'false')) { throw 'Transport must be true or false' }
# Parse every maintained Windows script, including the generated template.
Get-ChildItem install, tools, tests -Recurse -File | Where-Object { $_.Name -match '\.ps1(\.in)?$' } | ForEach-Object {
    $source = [IO.File]::ReadAllText($_.FullName).Replace('@TEST_MODE@', '$false')
    $tokens = $null; $errors = $null
    $null = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw "$($_.FullName): $errors" }
}
& tests/release/windows-installer-preflight.ps1
Invoke-Checked { cargo +1.98.0 clippy --locked --workspace --all-targets --all-features -- -D warnings }
Invoke-Checked { cargo +1.98.0 test --locked --workspace --all-features --no-fail-fast }
Invoke-Checked { & target/debug/voytty.exe }
if ($Transport -eq 'true') {
    Invoke-Checked { cargo +1.98.0 test --locked --manifest-path vendor/noq-proto/Cargo.toml --target-dir target/noq-tests --lib }
    Invoke-Checked { cargo +1.98.0 test --locked --manifest-path vendor/iroh/Cargo.toml --target-dir target/iroh-tests --no-default-features --features tls-ring,unstable-custom-transports --lib voytty_ }
}

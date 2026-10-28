# Native MSVC candidate owner. Main builds are cold; this never signs or publishes.
param([string]$OutputDirectory = 'release-input')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$target = $env:RELEASE_TARGET
$machine = switch ($target) {
    'x86_64-pc-windows-msvc' { 0x8664 }
    'aarch64-pc-windows-msvc' { 0xAA64 }
    default { throw "Unsupported Windows target: $target" }
}
$hostTriple = (& rustc +1.98.0 -vV | Select-String '^host: ').Line.Substring(6)
if ($LASTEXITCODE -ne 0 -or $hostTriple -cne $target) { throw 'Native Rust host does not match release target' }
$commit = & git rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $commit -cne $env:VOYTTY_SOURCE_COMMIT -or -not $env:SOURCE_DATE_EPOCH) { throw 'Exact source identity is required' }
& cargo +1.98.0 build --locked --release --package voytty-cli
if ($LASTEXITCODE -ne 0) { throw 'Native release build failed' }
$binary = [IO.Path]::GetFullPath('target/release/voytty.exe')
$bytes = [IO.File]::ReadAllBytes($binary)
$offset = [BitConverter]::ToInt32($bytes, 0x3C)
if ($bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A -or [BitConverter]::ToUInt32($bytes, $offset) -ne 0x4550 -or [BitConverter]::ToUInt16($bytes, $offset + 4) -ne $machine) { throw 'Native PE architecture mismatch' }
# PE32+ optional header: Magic at +0, SizeOfStackReserve at +72.
$optionalHeader = $offset + 24
if ([BitConverter]::ToUInt16($bytes, $optionalHeader) -ne 0x20B -or [BitConverter]::ToUInt64($bytes, $optionalHeader + 72) -ne 8388608) { throw 'Native PE stack reservation mismatch' }
$identity = & $binary --internal-release-self-check
if ($LASTEXITCODE -ne 0) { throw 'Candidate self-check failed' }
$record = $identity | ConvertFrom-Json
if ($record.build.target -cne $target -or $record.build.source_commit -cne $commit) { throw 'Candidate identity mismatch' }
[IO.Directory]::CreateDirectory([IO.Path]::GetFullPath($OutputDirectory)) | Out-Null
$candidate = Join-Path $OutputDirectory "voytty-$target.exe"
$identityPath = Join-Path $OutputDirectory "voytty-$target.identity.json"
if ((Test-Path -LiteralPath $candidate) -or (Test-Path -LiteralPath $identityPath)) { throw 'Candidate already exists' }
Copy-Item -LiteralPath $binary -Destination $candidate
[IO.File]::WriteAllText([IO.Path]::GetFullPath($identityPath), ($identity -join "`n"), (New-Object Text.UTF8Encoding($false)))
& $candidate
if ($LASTEXITCODE -ne 0) { throw 'Candidate CLI smoke check failed' }

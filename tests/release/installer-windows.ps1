# Final signed installer acceptance on each native Windows architecture.
param([Parameter(Mandatory)][string]$ReleaseDirectory, [Parameter(Mandatory)][string]$TestInstaller)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ReleaseDirectory = [IO.Path]::GetFullPath($ReleaseDirectory)
$TestInstaller = [IO.Path]::GetFullPath($TestInstaller)
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('voytty-https-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporary) | Out-Null
$server = $null; $certificate = $null
function Expect-Rejection([scriptblock]$Action, [string]$Message) {
    $rejected = $false
    try { & $Action } catch { $rejected = $true }
    if (-not $rejected) { throw $Message }
}
try {
    $openssl = Join-Path (Split-Path (Split-Path (Get-Command git.exe).Source)) 'usr/bin/openssl.exe'
    if (-not (Test-Path -LiteralPath $openssl)) { $openssl = (Get-Command openssl.exe).Source }
    $config = Join-Path $temporary 'certificate.conf'
    @'
[req]
prompt = no
distinguished_name = subject
x509_extensions = extensions
[subject]
CN = 127.0.0.1
[extensions]
subjectAltName = IP:127.0.0.1
basicConstraints = critical,CA:TRUE
keyUsage = critical,digitalSignature,keyEncipherment,keyCertSign
'@ | Set-Content -LiteralPath $config -Encoding Ascii
    $key = Join-Path $temporary 'server.key'; $cert = Join-Path $temporary 'server.crt'
    # Windows PowerShell 5 treats native stderr as errors under Stop, even
    # when OpenSSL succeeds. Capture its ordinary progress outside that pipe.
    $opensslLog = Join-Path $temporary 'openssl.log'
    $opensslProcess = Start-Process -FilePath $openssl -ArgumentList @('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-config', ('"' + $config + '"'), '-keyout', ('"' + $key + '"'), '-out', ('"' + $cert + '"')) -RedirectStandardError $opensslLog -NoNewWindow -Wait -PassThru
    if ($opensslProcess.ExitCode -ne 0) { throw ('Unable to generate fixture certificate: ' + [IO.File]::ReadAllText($opensslLog)) }
    $certificate = Import-Certificate -FilePath $cert -CertStoreLocation Cert:\CurrentUser\Root
    $port = Join-Path $temporary 'port'; $requests = Join-Path $temporary 'requests.log'
    $arguments = @([IO.Path]::GetFullPath('tests/release/https_fixture.py'), $ReleaseDirectory, $cert, $key, $port, $requests)
    $quoted = $arguments | ForEach-Object { '"' + $_.Replace('"', '\"') + '"' }
    $server = Start-Process -FilePath (Get-Command python.exe).Source -ArgumentList $quoted -PassThru -NoNewWindow
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $port)) {
        if ($server.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw 'HTTPS fixture did not start' }
        Start-Sleep -Milliseconds 50
    }
    $base = 'https://127.0.0.1:' + [IO.File]::ReadAllText($port)
    $directory = Join-Path $temporary (([char]0x7528).ToString() + [char]0x6237 + ' path\bin')
    $destination = Join-Path $directory 'voytty.exe'
    & $TestInstaller -InstallDir $directory -NoPath -TestBaseUrl $base
    if (-not (Test-Path -LiteralPath $destination)) { throw 'Installed executable is missing' }
    & $destination --internal-release-self-check
    if ($LASTEXITCODE -ne 0) { throw 'Installed self-check failed' }
    $digest = (Get-FileHash -LiteralPath $destination).Hash
    $before = [IO.File]::ReadAllText($requests)
    Expect-Rejection { & $TestInstaller -InstallDir $directory -NoPath -TestBaseUrl $base } 'Existing destination was overwritten'
    if ((Get-FileHash -LiteralPath $destination).Hash -ne $digest -or [IO.File]::ReadAllText($requests) -cne $before) { throw 'No-clobber rejection had side effects' }
    $badDirectory = Join-Path $temporary 'negative'
    Expect-Rejection { & (Join-Path $ReleaseDirectory 'voytty-install.ps1') -InstallDir $badDirectory -NoPath -TestBaseUrl $base } 'Production installer accepted a fixture URL'
    foreach ($asset in @('voytty-release.json', 'voytty-release.json.sig')) {
        $path = Join-Path $ReleaseDirectory $asset
        $original = [IO.File]::ReadAllBytes($path)
        try {
            $corrupt = $original.Clone(); $corrupt[0] = $corrupt[0] -bxor 1
            [IO.File]::WriteAllBytes($path, $corrupt)
            Expect-Rejection { & $TestInstaller -InstallDir $badDirectory -NoPath -TestBaseUrl $base } "Tampered $asset was accepted"
            if (Test-Path -LiteralPath (Join-Path $badDirectory 'voytty.exe')) { throw 'Failed verification installed an executable' }
        } finally { [IO.File]::WriteAllBytes($path, $original) }
    }
    Write-Output 'Windows authenticated installer acceptance passed.'
} finally {
    if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id; $server.WaitForExit() }
    if ($certificate) { Remove-Item -LiteralPath ("Cert:\CurrentUser\Root\" + $certificate.Thumbprint) }
    Remove-Item -LiteralPath $temporary -Recurse -Force
}

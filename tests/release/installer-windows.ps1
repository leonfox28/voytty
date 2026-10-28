# Final signed installer acceptance on each native Windows architecture.
param([string]$ReleaseDirectory, [string]$TestInstaller, [switch]$FixtureOnly)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted') {
    throw 'The HTTPS trust fixture requires a disposable GitHub-hosted Windows runner'
}
if (-not $FixtureOnly) {
    $ReleaseDirectory = [IO.Path]::GetFullPath($ReleaseDirectory)
    $TestInstaller = [IO.Path]::GetFullPath($TestInstaller)
}
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('voytty-https-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporary) | Out-Null
$server = $null; $certificate = $null
function Invoke-FixtureProcess([string]$FilePath, [string[]]$Arguments, [string]$Name) {
    $stdout = Join-Path $temporary ($Name + '.stdout')
    $stderr = Join-Path $temporary ($Name + '.stderr')
    $quoted = $Arguments | ForEach-Object { '"' + $_ + '"' }
    $process = Start-Process -FilePath $FilePath -ArgumentList $quoted -RedirectStandardOutput $stdout -RedirectStandardError $stderr -NoNewWindow -PassThru
    # Hold a process handle before exit so Windows PowerShell retains ExitCode.
    $null = $process.Handle
    try {
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            throw "$Name exceeded the 30-second fixture deadline"
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) {
            throw ("$Name failed: " + [IO.File]::ReadAllText($stdout) + [IO.File]::ReadAllText($stderr))
        }
        if ($Name -in @('trust', 'untrust')) { Write-Output ([IO.File]::ReadAllText($stdout)) }
    } finally { $process.Dispose() }
}
function Test-FixtureCertificate([string]$Thumbprint) {
    # The Certificate provider can retain a store opened before certutil ran.
    # Open a fresh store for each before/after assertion, including cleanup.
    $store = New-Object Security.Cryptography.X509Certificates.X509Store('Root', 'LocalMachine')
    try {
        $store.Open([Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
        return $store.Certificates.Find([Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint, $Thumbprint, $false).Count -gt 0
    } finally { $store.Dispose() }
}
function Expect-Rejection([scriptblock]$Action, [string]$Message) {
    $rejected = $false
    try { & $Action } catch { $rejected = $true }
    if (-not $rejected) { throw $Message }
}
try {
    if ($FixtureOnly) {
        $ReleaseDirectory = Join-Path $temporary 'fixture'
        [IO.Directory]::CreateDirectory($ReleaseDirectory) | Out-Null
        [IO.File]::WriteAllText((Join-Path $ReleaseDirectory 'health.txt'), 'voytty HTTPS fixture')
    }
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
    Write-Output 'Generating the temporary HTTPS certificate.'
    Invoke-FixtureProcess $openssl @('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-config', $config, '-keyout', $key, '-out', $cert) 'openssl'
    $generatedCertificate = New-Object Security.Cryptography.X509Certificates.X509Certificate2($cert)
    if (Test-FixtureCertificate $generatedCertificate.Thumbprint) { throw 'Fixture certificate already exists in the runner store' }
    $certificate = $generatedCertificate
    # CurrentUser Root requires an interactive CryptoAPI confirmation even with
    # certutil -user -f. Only this disposable hosted VM receives the fresh root;
    # its elevated LocalMachine store supports unattended import and cleanup.
    Write-Output 'Trusting the temporary certificate without an interactive dialog.'
    Invoke-FixtureProcess (Get-Command certutil.exe).Source @('-f', '-addstore', 'Root', $cert) 'trust'
    if (-not (Test-FixtureCertificate $certificate.Thumbprint)) { throw 'Fixture certificate was not imported' }
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
    Write-Output 'HTTPS fixture is listening.'
    if ($FixtureOnly) {
        Add-Type -AssemblyName System.Net.Http
        $client = New-Object Net.Http.HttpClient
        $client.Timeout = [TimeSpan]::FromSeconds(10)
        try {
            if ($client.GetStringAsync($base + '/health.txt').GetAwaiter().GetResult() -cne 'voytty HTTPS fixture') { throw 'HTTPS fixture response mismatch' }
        } finally { $client.Dispose() }
        Write-Output 'Windows HTTPS fixture acceptance passed with ordinary TLS validation.'
        return
    }
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
    if ($certificate) {
        if (Test-FixtureCertificate $certificate.Thumbprint) {
            Invoke-FixtureProcess (Get-Command certutil.exe).Source @('-f', '-delstore', 'Root', $certificate.Thumbprint) 'untrust'
        }
        if (Test-FixtureCertificate $certificate.Thumbprint) { throw 'Fixture certificate cleanup failed' }
        $certificate.Dispose()
        Write-Output 'Temporary certificate removed from the disposable runner trust store.'
    }
    Remove-Item -LiteralPath $temporary -Recurse -Force
}

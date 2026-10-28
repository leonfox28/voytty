# Windows bootstrap. The selected immutable Release owns the native installer.
param([string]$Version = '', [string]$InstallDir = '', [switch]$NoPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
if ($Version -and $Version -notmatch '^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$') { throw 'Version must be a release version, for example 0.2.0' }
$origin = 'https://github.com/leonfox28/voytty/releases'
$url = if ($Version) { "$origin/download/v$($Version.TrimStart('v'))/voytty-install.ps1" } else { "$origin/latest/download/voytty-install.ps1" }
Add-Type -AssemblyName System.Net.Http
$handler = New-Object Net.Http.HttpClientHandler
$handler.AllowAutoRedirect = $false
$client = New-Object Net.Http.HttpClient($handler)
$client.Timeout = [TimeSpan]::FromSeconds(60)
$stream = $null; $response = $null
$memory = New-Object IO.MemoryStream
$watch = [Diagnostics.Stopwatch]::StartNew()
$deadline = New-Object Threading.CancellationTokenSource
$deadline.CancelAfter(60000)
try {
    for ($redirect = 0; ; $redirect++) {
        if (([uri]$url).Scheme -ne 'https') { throw 'HTTPS is required' }
        $response = $client.GetAsync($url, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $deadline.Token).GetAwaiter().GetResult()
        if ([int]$response.StatusCode -notin @(301, 302, 303, 307, 308)) { break }
        if ($redirect -ge 5 -or -not $response.Headers.Location) { throw 'Invalid installer redirect' }
        $url = (New-Object Uri([uri]$url, $response.Headers.Location)).AbsoluteUri
        $response.Dispose()
    }
    $response.EnsureSuccessStatusCode() | Out-Null
    $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
    $buffer = New-Object byte[] 8192
    while ($true) {
        $read = $stream.ReadAsync($buffer, 0, $buffer.Length, $deadline.Token)
        $remaining = [int][Math]::Max(1, [Math]::Min(30000, 60000 - $watch.ElapsedMilliseconds))
        if (-not $read.Wait($remaining)) { throw 'Installer download stalled or exceeded its total deadline' }
        $length = $read.GetAwaiter().GetResult()
        if ($length -eq 0) { break }
        if ($memory.Length + $length -gt 262144 -or $watch.Elapsed.TotalSeconds -gt 60) { throw 'Installer download exceeded its bound' }
        $memory.Write($buffer, 0, $length)
    }
    $installer = [scriptblock]::Create([Text.Encoding]::UTF8.GetString($memory.ToArray()))
} finally {
    $deadline.Cancel(); $deadline.Dispose()
    if ($stream) { $stream.Dispose() }; if ($response) { $response.Dispose() }
    $memory.Dispose(); $client.Dispose(); $handler.Dispose()
}
$arguments = @{ NoPath = $NoPath }
if ($InstallDir) { $arguments.InstallDir = $InstallDir }
& $installer @arguments

# Execute the actual installer preflight, Unicode archive extraction, and type resolution in Windows PowerShell 5.1.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Net.Http
function Write-TestTarGz([string]$Path, [hashtable[]]$Entries, [int]$TrailingZeroBlocks = 2) {
    $file = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $gzip = New-Object IO.Compression.GZipStream($file, [IO.Compression.CompressionMode]::Compress)
        try {
            foreach ($entry in $Entries) {
                $header = New-Object byte[] 512
                $nameBytes = [Text.Encoding]::UTF8.GetBytes([string]$entry.Name)
                [Array]::Copy($nameBytes, 0, $header, 0, $nameBytes.Length)
                $data = [byte[]]$entry.Data
                $sizeBytes = [Text.Encoding]::ASCII.GetBytes([Convert]::ToString($data.Length, 8).PadLeft(11, [char]'0'))
                [Array]::Copy($sizeBytes, 0, $header, 124, $sizeBytes.Length)
                for ($i = 148; $i -lt 156; $i++) { $header[$i] = 0x20 }
                $header[156] = if ($entry.ContainsKey('TypeFlag')) { [byte]$entry.TypeFlag } else { [byte][char]'0' }
                if ($entry.ContainsKey('UstarPrefix') -and $entry.UstarPrefix) {
                    $magicBytes = [Text.Encoding]::ASCII.GetBytes('ustar')
                    [Array]::Copy($magicBytes, 0, $header, 257, $magicBytes.Length)
                    $header[262] = 0; $header[263] = [byte][char]'0'; $header[264] = [byte][char]'0'
                    $prefixBytes = [Text.Encoding]::UTF8.GetBytes([string]$entry.UstarPrefix)
                    [Array]::Copy($prefixBytes, 0, $header, 345, $prefixBytes.Length)
                } else {
                    $magicBytes = [Text.Encoding]::ASCII.GetBytes('ustar  ')
                    [Array]::Copy($magicBytes, 0, $header, 257, $magicBytes.Length)
                }
                $sum = 0
                for ($i = 0; $i -lt 512; $i++) { $sum += [int]$header[$i] }
                if ($entry.ContainsKey('BadChecksum') -and $entry.BadChecksum) { $sum += 1 }
                $checksumBytes = [Text.Encoding]::ASCII.GetBytes([Convert]::ToString($sum, 8).PadLeft(6, [char]'0'))
                [Array]::Copy($checksumBytes, 0, $header, 148, $checksumBytes.Length)
                $header[154] = 0; $header[155] = 0x20
                $gzip.Write($header, 0, 512)
                if ($data.Length -gt 0) {
                    $gzip.Write($data, 0, $data.Length)
                    $padding = (512 - ($data.Length % 512)) % 512
                    if ($padding -gt 0) {
                        $pad = New-Object byte[] $padding
                        $gzip.Write($pad, 0, $padding)
                    }
                }
            }
            if ($TrailingZeroBlocks -gt 0) {
                $zeros = New-Object byte[] ($TrailingZeroBlocks * 512)
                $gzip.Write($zeros, 0, $zeros.Length)
            }
        } finally { $gzip.Dispose() }
    } finally { $file.Dispose() }
}
function Expect-Failure([scriptblock]$Action, [string]$Message) {
    $failed = $false
    try { & $Action } catch { $failed = $true }
    if (-not $failed) { throw $Message }
}
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('voytty-preflight-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporary) | Out-Null
try {
    $targets = @(@{ target = 'x86_64-pc-windows-msvc' }, @{ target = 'aarch64-pc-windows-msvc' })
    $source = [IO.File]::ReadAllText([IO.Path]::GetFullPath('install/versioned.ps1.in'))
    # The no-clobber preflight must finish before any download. If it regresses,
    # this non-HTTPS sentinel is rejected before making a network request.
    $rendered = $source.Replace('@TAG@', 'v0.0.0').Replace('@BASE_URL@', 'http://127.0.0.1:1').Replace('@MANIFEST_SHA256@', ('0' * 64)).Replace('@TEST_MODE@', '$false').Replace('@ARTIFACTS@', (ConvertTo-Json -InputObject $targets -Compress))
    $script = Join-Path $temporary 'installer.ps1'
    [IO.File]::WriteAllText($script, $rendered)
    $destination = Join-Path $temporary 'voytty.exe'
    [IO.File]::WriteAllText($destination, 'existing fixture executable')
    $failure = $null
    try { & $script -InstallDir $temporary -NoPath } catch { $failure = $_.Exception.Message }
    if ($failure -cne 'voytty.exe already exists; use voytty update') {
        throw "Installer failed before its no-clobber boundary: $failure"
    }
    if ([IO.File]::ReadAllText($destination) -cne 'existing fixture executable') { throw 'Installer changed an existing executable' }
    # Exercise private staging and native archive extraction under a non-ASCII Unicode path.
    $unicodeDir = Join-Path $temporary (([char]0x7528).ToString() + [char]0x6237 + ' path\bin')
    [IO.Directory]::CreateDirectory($unicodeDir) | Out-Null
    $staging = Join-Path $unicodeDir ('.voytty-install-' + [guid]::NewGuid().ToString('N'))
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetOwner($sid)
    $acl.SetAccessRuleProtection($true, $false)
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
    [VoyttyInstallerPlatform]::CreatePrivateDirectory($staging, $acl.GetSecurityDescriptorBinaryForm())
    $payload = [Text.Encoding]::UTF8.GetBytes(('voytty unicode tar payload ' * 40))
    $validArchive = Join-Path $staging 'voytty-x86_64-pc-windows-msvc.tar.gz'
    Write-TestTarGz $validArchive @(@{ Name = 'voytty.exe'; Data = $payload })
    $extracted = Join-Path $staging 'voytty.exe'
    [VoyttyInstallerPlatform]::ExtractSingleFileTarGz($validArchive, 'voytty.exe', $extracted, 268435456L)
    if ([Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($extracted)) -cne [Text.Encoding]::UTF8.GetString($payload)) {
        throw 'Extracted Unicode-path archive payload mismatch'
    }
    $exactBlockPayload = [Text.Encoding]::ASCII.GetBytes(('x' * 512))
    $exactBlockArchive = Join-Path $staging 'exact-block.tar.gz'
    Write-TestTarGz $exactBlockArchive @(@{ Name = 'voytty.exe'; Data = $exactBlockPayload })
    $exactBlockOutput = Join-Path $staging 'voytty-exact.exe'
    [VoyttyInstallerPlatform]::ExtractSingleFileTarGz($exactBlockArchive, 'voytty.exe', $exactBlockOutput, 268435456L)
    if ([Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($exactBlockOutput)) -cne [Text.Encoding]::ASCII.GetString($exactBlockPayload)) {
        throw 'Extracted block-aligned archive payload mismatch'
    }
    Expect-Failure { [VoyttyInstallerPlatform]::ExtractSingleFileTarGz($validArchive, 'voytty.exe', $extracted, 268435456L) } 'Extractor overwrote an existing candidate file'
    if ([Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($extracted)) -cne [Text.Encoding]::UTF8.GetString($payload)) {
        throw 'Existing candidate file changed after no-clobber extraction failure'
    }
    Expect-Failure { [VoyttyInstallerPlatform]::ExtractSingleFileTarGz($validArchive, 'voytty.exe', (Join-Path $staging 'oversize.exe'), [long]($payload.Length - 1)) } 'Extractor accepted an oversized entry'
    $negatives = @(
        @{ Name = 'wrong-name.tar.gz'; Entries = @(@{ Name = 'other.exe'; Data = $payload }); Zeros = 2 },
        @{ Name = 'ustar-prefix.tar.gz'; Entries = @(@{ Name = 'voytty.exe'; Data = $payload; UstarPrefix = 'sub' }); Zeros = 2 },
        @{ Name = 'extra-entry.tar.gz'; Entries = @(@{ Name = 'voytty.exe'; Data = $payload }, @{ Name = 'extra.txt'; Data = [Text.Encoding]::ASCII.GetBytes('extra') }); Zeros = 2 },
        @{ Name = 'symlink.tar.gz'; Entries = @(@{ Name = 'voytty.exe'; Data = $payload; TypeFlag = [byte][char]'2' }); Zeros = 2 },
        @{ Name = 'bad-checksum.tar.gz'; Entries = @(@{ Name = 'voytty.exe'; Data = $payload; BadChecksum = $true }); Zeros = 2 },
        @{ Name = 'no-trailer.tar.gz'; Entries = @(@{ Name = 'voytty.exe'; Data = $payload }); Zeros = 0 }
    )
    foreach ($case in $negatives) {
        $badArchive = Join-Path $staging $case.Name
        Write-TestTarGz $badArchive $case.Entries $case.Zeros
        $badOutput = Join-Path $staging ($case.Name + '.exe')
        Expect-Failure { [VoyttyInstallerPlatform]::ExtractSingleFileTarGz($badArchive, 'voytty.exe', $badOutput, 268435456L) } "Extractor accepted invalid archive: $($case.Name)"
        if (Test-Path -LiteralPath $badOutput) { throw "Failed extraction left output behind: $($case.Name)" }
    }
    # Parsing alone accepts unknown accelerators such as [ushort] on PS 5.1.
    # The real preflight has now loaded its declared interop type; resolve every
    # static type used by both entry points in this exact PowerShell runtime.
    foreach ($inputSource in @($rendered, [IO.File]::ReadAllText([IO.Path]::GetFullPath('install/install.ps1')))) {
        $tokens = $null; $errors = $null
        $tree = [Management.Automation.Language.Parser]::ParseInput($inputSource, [ref]$tokens, [ref]$errors)
        if ($errors.Count) { throw "Installer syntax errors: $errors" }
        $types = $tree.FindAll({ param($node) $node -is [Management.Automation.Language.TypeExpressionAst] -or $node -is [Management.Automation.Language.TypeConstraintAst] }, $true)
        foreach ($node in $types) {
            if ($null -eq $node.TypeName.GetReflectionType()) { throw "Installer type is unavailable in this runtime: $($node.TypeName.FullName)" }
        }
    }
    Write-Output "Installer native-architecture preflight, Unicode extraction, and runtime types passed in PowerShell $($PSVersionTable.PSVersion)."
} finally { Remove-Item -LiteralPath $temporary -Recurse -Force }

# Execute the actual installer preflight and resolve its types in Windows PowerShell 5.1.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Net.Http
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
    Write-Output "Installer native-architecture preflight and runtime types passed in PowerShell $($PSVersionTable.PSVersion)."
} finally { Remove-Item -LiteralPath $temporary -Recurse -Force }

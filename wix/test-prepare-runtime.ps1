[CmdletBinding()]
param(
    [string]$SevenZip = (Get-Command 7z.exe -ErrorAction Stop).Source
)

$ErrorActionPreference = 'Stop'
$prepare = Join-Path $PSScriptRoot 'prepare-runtime.ps1'
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) "hop-runtime-test-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $temporary | Out-Null
$downloads = @{}
$downloadCount = 0

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-Fails([scriptblock]$Action, [string]$Pattern) {
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_.Exception.Message }
    Assert-True ($null -ne $failure -and $failure -like $Pattern) "Expected error '$Pattern', got '$failure'."
}

# The preparation script resolves this test-local downloader instead of making
# network requests. Hash checks and 7-Zip operations still run for real.
function Invoke-WebRequest([string]$Uri, [string]$OutFile) {
    if (-not $downloads.ContainsKey($Uri)) { throw "Unexpected test download: $Uri" }
    $script:downloadCount++
    Copy-Item -LiteralPath $downloads[$Uri] -Destination $OutFile
}

function Artifact([string]$Name, [string]$Text) {
    $path = Join-Path $temporary $Name
    Set-Content -LiteralPath $path -Value $Text -Encoding utf8
    $url = "https://runtime-fixture.invalid/$Name"
    $downloads[$url] = $path
    return @{
        name = $Name
        url = $url
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    }
}

function Save-Manifest($Value) {
    $path = Join-Path $temporary 'manifest.json'
    $Value | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $path -Encoding utf8
    return $path
}

try {
    $upstream = Join-Path $temporary 'input'
    New-Item -ItemType Directory -Path $upstream | Out-Null
    Set-Content -LiteralPath (Join-Path $upstream 'libmpv-2.dll') -Value 'TEST ONLY: not a real runtime'
    $archive = Join-Path $temporary 'upstream.7z'
    Push-Location $upstream
    try {
        & $SevenZip a -t7z $archive '.\*' | Out-Host
        if ($LASTEXITCODE -ne 0) { throw 'Cannot create test archive.' }
    } finally { Pop-Location }
    $runtimeUrl = 'https://runtime-fixture.invalid/runtime.7z'
    $downloads[$runtimeUrl] = $archive
    $license = Artifact 'TEST-LICENSE.txt' 'Test fixture license material; not an mpv license.'
    $source = Artifact 'TEST-SOURCE.txt' 'Test fixture source material; not mpv source.'
    $source.purpose = 'source'
    $build = Artifact 'TEST-BUILD.txt' 'Test fixture build material; not mpv build instructions.'
    $build.purpose = 'build'
    $data = @{
        schemaVersion = 1
        review = @{ approved = $true; reviewer = 'Test fixture'; date = '2026-10-22'; notes = 'TEST ONLY' }
        runtime = @{
            name = 'TEST-ONLY'
            url = $runtimeUrl
            sha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
            root = '.'
            files = @('libmpv-2.dll')
        }
        licenses = @($license)
        sources = @($source, $build)
        components = @(@{
            name = 'Test component'
            version = 'test-revision'
            license = 'TEST-ONLY'
            licenses = @($license.name)
            sources = @($source.name, $build.name)
        })
    }

    $data.review.approved = $false
    $manifest = Save-Manifest $data
    $pending = & $prepare -Manifest $manifest -ValidateOnly
    Assert-True (-not $pending.Approved) 'Unapproved manifests must report pending review.'
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'unapproved') } '*not approved*'
    Assert-True ($downloadCount -eq 0) 'Unapproved manifests must not download anything.'
    $data.review.approved = $true
    $manifest = Save-Manifest $data
    Assert-True ((& $prepare -Manifest $manifest -ValidateOnly).Approved) 'Complete reviewed manifest should validate.'

    $result = & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'ready') -SevenZip $SevenZip
    foreach ($name in @('libmpv-2.dll', 'THIRD-PARTY-NOTICES.txt', 'SOURCE.txt', 'runtime-manifest.json')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $result.RuntimeDirectory $name)) "Missing $name"
    }
    $notice = Get-Content -LiteralPath (Join-Path $result.RuntimeDirectory 'THIRD-PARTY-NOTICES.txt') -Raw
    $sourceNotice = Get-Content -LiteralPath (Join-Path $result.RuntimeDirectory 'SOURCE.txt') -Raw
    Assert-True ($notice.Contains('Test component') -and $notice.Contains($license.name)) 'Notices must identify components and license files.'
    Assert-True ($sourceNotice.Contains($source.sha256) -and $sourceNotice.Contains($build.url)) 'Source notice must include source and build provenance.'
    foreach ($entry in @($license, $source, $build)) {
        $folder = if ($entry -eq $license) { 'licenses' } else { 'sources' }
        $path = Join-Path (Join-Path $result.RuntimeDirectory $folder) $entry.name
        Assert-True ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -eq $entry.sha256) 'Materials must be preserved unchanged.'
    }
    Assert-True ((Get-FileHash -LiteralPath $result.Archive -Algorithm SHA256).Hash -eq $result.Sha256) 'Bundle hash must match.'
    Assert-True ((Get-Content -LiteralPath "$($result.Archive).sha256" -Raw).Contains($result.Sha256)) 'Checksum sidecar must match.'
    $extracted = Join-Path $temporary 'roundtrip'
    & $SevenZip x $result.Archive "-o$extracted" -y | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract prepared test bundle.' }
    Assert-True (Test-Path -LiteralPath (Join-Path $extracted 'libmpv-2.dll')) 'Bundle must not have an enclosing directory.'
    Assert-True (Test-Path -LiteralPath (Join-Path $extracted "sources\$($source.name)")) 'Bundle must include source materials.'
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'ready') -SevenZip $SevenZip } '*must not exist*'

    $data.runtime.sha256 = '0' * 64
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'bad-hash') -SevenZip $SevenZip } '*Checksum mismatch*'
    $data.runtime.sha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    $data.licenses[0].sha256 = '0' * 64
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'bad-license') -SevenZip $SevenZip } '*Checksum mismatch*'
    $data.licenses[0].sha256 = (Get-FileHash -LiteralPath $downloads[$license.url] -Algorithm SHA256).Hash
    $data.sources[0].sha256 = '0' * 64
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'bad-source') -SevenZip $SevenZip } '*Checksum mismatch*'
    $data.sources[0].sha256 = (Get-FileHash -LiteralPath $downloads[$source.url] -Algorithm SHA256).Hash
    $data.runtime.files = @('libmpv-2.dll', 'missing-dependency.dll')
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'missing-dll') -SevenZip $SevenZip } '*Missing or empty runtime DLL*'
    $data.runtime.files = @('libmpv-2.dll')
    $downloads[$runtimeUrl] = $downloads[$source.url]
    $data.runtime.sha256 = $source.sha256
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -OutputDirectory (Join-Path $temporary 'corrupt-archive') -SevenZip $SevenZip } '*Runtime extraction failed*'
    $downloads[$runtimeUrl] = $archive
    $data.runtime.sha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash

    $data.components[0].sources = @('missing.tar.xz')
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -ValidateOnly } '*Unknown artifact reference*'
    $data.components[0].sources = @($source.name, $build.name)
    $data.runtime.files = @('libmpv-2.dll', '..\escape.dll')
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -ValidateOnly } '*escapes*'
    $data.runtime.files = @('libmpv-2.dll')
    $data.runtime.url = 'http://runtime-fixture.invalid/runtime.7z'
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -ValidateOnly } '*HTTPS*'
    $data.runtime.url = $runtimeUrl
    $data.sources[1].purpose = 'source'
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -ValidateOnly } '*purpose source and build*'
    $data.sources[1].purpose = 'build'
    $data.sources = @()
    $manifest = Save-Manifest $data
    Assert-Fails { & $prepare -Manifest $manifest -ValidateOnly } '*sources must not be empty*'
    Write-Host 'Runtime preparation tests passed (no external downloads or licensing approval).'
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
# Expected failures above include native 7-Zip errors. GitHub's PowerShell
# wrapper propagates LASTEXITCODE, so clear it only after all assertions pass.
$global:LASTEXITCODE = 0

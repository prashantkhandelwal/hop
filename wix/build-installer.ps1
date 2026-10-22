[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$RuntimeDirectory,
    [string]$WixBin = $(if ($env:WIX) { Join-Path $env:WIX 'bin' } else { '' }),
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$runtime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
if (-not (Test-Path -LiteralPath $runtime -PathType Container)) {
    throw 'RuntimeDirectory must be a folder.'
}
foreach ($required in @('libmpv-2.dll', 'THIRD-PARTY-NOTICES.txt', 'SOURCE.txt')) {
    $file = Join-Path $runtime $required
    if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or (Get-Item -LiteralPath $file).Length -eq 0) {
        throw "Approved runtime bundle must contain a nonempty $required at its root."
    }
}
foreach ($reserved in @('hop.exe', 'LICENSE')) {
    if (Test-Path -LiteralPath (Join-Path $runtime $reserved)) {
        throw "Runtime bundle must not contain reserved Hop file $reserved."
    }
}
if (Get-ChildItem -LiteralPath $runtime -Recurse -Force | Where-Object {
    $_.Attributes -band [System.IO.FileAttributes]::ReparsePoint
}) {
    throw 'Runtime bundles must not contain symbolic links or junctions.'
}

$stream = [System.IO.File]::OpenRead((Join-Path $runtime 'libmpv-2.dll'))
$reader = [System.IO.BinaryReader]::new($stream)
try {
    if ($reader.ReadUInt16() -ne 0x5A4D) { throw 'libmpv-2.dll is not a PE file.' }
    $stream.Position = 0x3C
    $offset = $reader.ReadUInt32()
    $stream.Position = $offset
    if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) {
        throw 'The installer requires an x86-64 libmpv-2.dll.'
    }
} finally {
    $reader.Dispose()
}

if (-not $WixBin) {
    $WixBin = Split-Path -Parent (Get-Command candle.exe -ErrorAction Stop).Source
}
$WixBin = (Resolve-Path -LiteralPath $WixBin).Path
foreach ($tool in @('candle.exe', 'light.exe', 'heat.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $WixBin $tool))) {
        throw "WiX Toolset 3.14.1 is required; missing $tool in $WixBin."
    }
}
Push-Location $project
try {
    $metadata = cargo metadata --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Cargo metadata.' }
    $target = ($metadata | ConvertFrom-Json).target_directory
    $build = Join-Path $target 'installer'
    New-Item -ItemType Directory -Force -Path $build | Out-Null
    if (-not $OutputDirectory) { $OutputDirectory = Join-Path $target 'wix' }
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    $OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path

    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Hop release build failed.' }

    $fragment = Join-Path $build 'runtime.wxs'
    & (Join-Path $WixBin 'heat.exe') dir $runtime -nologo -ag -srd -sreg -scom `
        -cg MpvRuntime -dr INSTALLFOLDER -var var.MpvRuntimeDir -out $fragment
    if ($LASTEXITCODE -ne 0) { throw 'Cannot harvest the approved runtime bundle.' }
    # Per-user components need HKCU key paths, not file key paths (MSI ICE38).
    [xml]$runtimeXml = Get-Content -LiteralPath $fragment -Raw
    $namespace = 'http://schemas.microsoft.com/wix/2006/wi'
    $manager = [System.Xml.XmlNamespaceManager]::new($runtimeXml.NameTable)
    $manager.AddNamespace('w', $namespace)
    foreach ($component in $runtimeXml.SelectNodes('//w:Component', $manager)) {
        $source = $component.SelectSingleNode('w:File', $manager).GetAttribute('Source')
        $identity = $source.Replace('$(var.MpvRuntimeDir)', 'hop-runtime').ToLowerInvariant()
        $hash = [System.Security.Cryptography.SHA256]::Create()
        try {
            $bytes = $hash.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($identity))
            $guid = [guid]::new([byte[]]$bytes[0..15])
        } finally {
            $hash.Dispose()
        }
        $component.SetAttribute('Guid', $guid.ToString().ToUpperInvariant())
        foreach ($file in $component.SelectNodes('w:File', $manager)) {
            $file.RemoveAttribute('KeyPath')
        }
        $registry = $runtimeXml.CreateElement('RegistryValue', $namespace)
        $registry.SetAttribute('Root', 'HKCU')
        $registry.SetAttribute('Key', 'Software\Hop\Installer\Runtime')
        $registry.SetAttribute('Name', $component.GetAttribute('Id'))
        $registry.SetAttribute('Type', 'integer')
        $registry.SetAttribute('Value', '1')
        $registry.SetAttribute('KeyPath', 'yes')
        $component.AppendChild($registry) | Out-Null
    }
    foreach ($directory in $runtimeXml.SelectNodes('//w:Directory', $manager)) {
        $component = $runtimeXml.SelectSingleNode('//w:Component', $manager)
        if ($component) {
            $remove = $runtimeXml.CreateElement('RemoveFolder', $namespace)
            $remove.SetAttribute('Id', "remove_$($directory.GetAttribute('Id'))")
            $remove.SetAttribute('Directory', $directory.GetAttribute('Id'))
            $remove.SetAttribute('On', 'uninstall')
            $component.AppendChild($remove) | Out-Null
        }
    }
    $runtimeXml.Save($fragment)

    $license = [System.IO.File]::ReadAllText((Join-Path $project 'LICENSE'))
    $escaped = $license.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
    $escaped = $escaped -replace '\r?\n', '\par '
    $rtf = Join-Path $build 'license.rtf'
    [System.IO.File]::WriteAllText($rtf, "{\rtf1\ansi $escaped}", [System.Text.Encoding]::ASCII)

    # ICE91 warns about per-user files in a per-machine install. This MSI is
    # deliberately per-user only; keep all other installer validation enabled.
    cargo wix --no-build --nocapture --bin-path $WixBin --include $fragment `
        --target-bin-dir (Join-Path $target 'release') --output "$OutputDirectory\" `
        --compiler-arg "-dMpvRuntimeDir=$runtime" `
        --compiler-arg "-dHopProjectDir=$project" `
        --compiler-arg "-dHopLicenseRtf=$rtf" `
        --linker-arg '-sice:ICE91'
    if ($LASTEXITCODE -ne 0) { throw 'MSI creation failed.' }
} finally {
    Pop-Location
}

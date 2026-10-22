[CmdletBinding()]
param(
    [string]$Manifest = (Join-Path $PSScriptRoot 'runtime-manifest.json'),
    [string]$OutputDirectory,
    [string]$SevenZip,
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'

function Require-Text($Value, [string]$Name) {
    if ($Value -isnot [string] -or [string]::IsNullOrWhiteSpace($Value)) {
        throw "$Name must be a nonempty string."
    }
}

function Assert-Download($Entry, [string]$Name) {
    Require-Text $Entry.url "$Name.url"
    $uri = $null
    if (-not [uri]::TryCreate($Entry.url, [UriKind]::Absolute, [ref]$uri) -or
        $uri.Scheme -ne 'https' -or $uri.UserInfo) {
        throw "$Name.url must be an absolute HTTPS URL without credentials."
    }
    if ($Entry.sha256 -isnot [string] -or $Entry.sha256 -notmatch '^[a-fA-F0-9]{64}$') {
        throw "$Name.sha256 must be a 64-character SHA-256."
    }
}

function Resolve-ContainedPath([string]$Root, [string]$Relative) {
    Require-Text $Relative 'Relative path'
    if ([System.IO.Path]::IsPathRooted($Relative) -or $Relative.Contains(':')) {
        throw "Path must be relative: $Relative"
    }
    $base = [System.IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $path = [System.IO.Path]::GetFullPath((Join-Path $base $Relative))
    if ($path -ne $base -and -not $path.StartsWith(
        "$base$([System.IO.Path]::DirectorySeparatorChar)", [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Path escapes its directory: $Relative"
    }
    return $path
}

function Receive-VerifiedFile($Entry, [string]$Destination) {
    Invoke-WebRequest -Uri $Entry.url -OutFile $Destination
    if ((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash -ne $Entry.sha256) {
        throw "Checksum mismatch for $($Entry.url)"
    }
}

$Manifest = (Resolve-Path -LiteralPath $Manifest).Path
$data = Get-Content -LiteralPath $Manifest -Raw | ConvertFrom-Json
if ($data.schemaVersion -ne 1 -or $data.review.approved -isnot [bool]) {
    throw 'Unsupported manifest schema or missing boolean review.approved.'
}
if (-not $data.review.approved) {
    $reason = 'Runtime manifest is not approved. Complete and review its dependency, license, source, and build-material inventory before enabling MSI preparation.'
    if ($ValidateOnly) {
        return [pscustomobject]@{ Approved = $false; Reason = $reason }
    }
    throw $reason
}
Require-Text $data.review.reviewer 'review.reviewer'
Require-Text $data.review.notes 'review.notes'
Require-Text $data.review.date 'review.date'
if ($data.review.date -notmatch '^\d{4}-\d{2}-\d{2}$') {
    throw 'review.date must use YYYY-MM-DD.'
}
$null = [datetime]::ParseExact($data.review.date, 'yyyy-MM-dd', [cultureinfo]::InvariantCulture)
Require-Text $data.runtime.name 'runtime.name'
Assert-Download $data.runtime 'runtime'
Require-Text $data.runtime.root 'runtime.root'
$null = Resolve-ContainedPath ([System.IO.Path]::GetTempPath()) $data.runtime.root
if (@($data.runtime.files).Count -eq 0 -or 'libmpv-2.dll' -notin $data.runtime.files) {
    throw 'runtime.files must explicitly include libmpv-2.dll.'
}
foreach ($path in $data.runtime.files) {
    $null = Resolve-ContainedPath ([System.IO.Path]::GetTempPath()) $path
    if ($path -notmatch '\.dll$') {
        throw "runtime.files must contain DLLs only: $path"
    }
}

$artifacts = @{}
foreach ($group in @('licenses', 'sources')) {
    if (@($data.$group).Count -eq 0) { throw "$group must not be empty." }
    foreach ($entry in $data.$group) {
        Require-Text $entry.name "$group.name"
        if ($entry.name -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]*$' -or $entry.name.EndsWith('.')) {
            throw "Artifact names must be plain filenames: $($entry.name)"
        }
        $key = "$group/$($entry.name)"
        if ($artifacts.ContainsKey($key)) { throw "Duplicate artifact: $key" }
        Assert-Download $entry $key
        $artifacts[$key] = $entry
    }
}
if ('build' -notin @($data.sources.purpose) -or 'source' -notin @($data.sources.purpose)) {
    throw 'sources must include entries with purpose source and build (recipes, patches, and instructions).'
}
if (@($data.components).Count -eq 0) { throw 'components must describe the reviewed dependency inventory.' }
$componentNames = @{}
$referenced = @{}
foreach ($component in $data.components) {
    foreach ($field in @('name', 'version', 'license')) {
        Require-Text $component.$field "components.$field"
    }
    if ($componentNames.ContainsKey($component.name)) { throw "Duplicate component: $($component.name)" }
    $componentNames[$component.name] = $true
    foreach ($group in @('licenses', 'sources')) {
        if (@($component.$group).Count -eq 0) {
            throw "$($component.name) must reference $group."
        }
        foreach ($name in $component.$group) {
            Require-Text $name "$($component.name).$group"
            $key = "$group/$name"
            if (-not $artifacts.ContainsKey($key)) { throw "Unknown artifact reference: $key" }
            $referenced[$key] = $true
        }
    }
}
foreach ($key in $artifacts.Keys) {
    if (-not $referenced.ContainsKey($key)) { throw "Artifact is not assigned to a component: $key" }
}
if ($ValidateOnly) {
    return [pscustomobject]@{ Approved = $true; Reason = 'Approved runtime manifest passed validation.' }
}
Require-Text $OutputDirectory 'OutputDirectory'
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) {
    throw 'OutputDirectory must not exist; use a fresh directory to avoid stale bundle contents.'
}
if (-not $SevenZip) { $SevenZip = (Get-Command 7z.exe -ErrorAction Stop).Source }
$SevenZip = (Resolve-Path -LiteralPath $SevenZip).Path
New-Item -ItemType Directory -Path $output | Out-Null
$archive = Join-Path $output 'upstream.7z'
Receive-VerifiedFile $data.runtime $archive
$extracted = Join-Path $output 'upstream'
& $SevenZip x $archive "-o$extracted" -y | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Runtime extraction failed with exit code $LASTEXITCODE." }
if (Get-ChildItem -LiteralPath $extracted -Recurse -Force | Where-Object {
    $_.Attributes -band [System.IO.FileAttributes]::ReparsePoint
}) {
    throw 'Runtime archives must not contain symbolic links or junctions.'
}
$root = Resolve-ContainedPath $extracted $data.runtime.root
$runtime = Join-Path $output 'runtime'
New-Item -ItemType Directory -Path $runtime | Out-Null
foreach ($path in $data.runtime.files) {
    $source = Resolve-ContainedPath $root $path
    if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or (Get-Item -LiteralPath $source).Length -eq 0) {
        throw "Missing or empty runtime DLL: $path"
    }
    $destination = Resolve-ContainedPath $runtime $path
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination
}
foreach ($group in @('licenses', 'sources')) {
    $directory = Join-Path $runtime $group
    New-Item -ItemType Directory -Path $directory | Out-Null
    foreach ($entry in $data.$group) {
        $destination = Join-Path $directory $entry.name
        Receive-VerifiedFile $entry $destination
        if ((Get-Item -LiteralPath $destination).Length -eq 0) {
            throw "Empty $group artifact: $($entry.name)"
        }
    }
}

$provenance = @(
    "Runtime: $($data.runtime.name)"
    "Binary URL: $($data.runtime.url)"
    "Binary SHA-256: $($data.runtime.sha256)"
    "Reviewed by: $($data.review.reviewer)"
    "Review date: $($data.review.date)"
    "Review notes: $($data.review.notes)"
)
$notices = @('Third-party runtime distributed with Hop', '') + $provenance
$sources = @('Matching sources and build materials distributed with this runtime', '') + $provenance
foreach ($component in $data.components) {
    $notices += @('', "Component: $($component.name)", "Version/revision: $($component.version)",
        "License: $($component.license)", 'License files:')
    $notices += @($component.licenses | ForEach-Object { "  licenses/$_" })
    $sources += @('', "Component: $($component.name)", "Version/revision: $($component.version)")
    foreach ($name in $component.sources) {
        $entry = $artifacts["sources/$name"]
        $sources += @("  sources/$name", "  Purpose: $($entry.purpose)",
            "  URL: $($entry.url)", "  SHA-256: $($entry.sha256)")
    }
}
Set-Content -LiteralPath (Join-Path $runtime 'THIRD-PARTY-NOTICES.txt') -Value $notices -Encoding utf8
Set-Content -LiteralPath (Join-Path $runtime 'SOURCE.txt') -Value $sources -Encoding utf8
Copy-Item -LiteralPath $Manifest -Destination (Join-Path $runtime 'runtime-manifest.json')
$bundle = Join-Path $output 'hop-mpv-runtime.7z'
Push-Location $runtime
try {
    & $SevenZip a -t7z $bundle '.\*' | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "Runtime packaging failed with exit code $LASTEXITCODE." }
} finally {
    Pop-Location
}
$checksum = (Get-FileHash -LiteralPath $bundle -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath "$bundle.sha256" -Value "$checksum  hop-mpv-runtime.7z" -Encoding ascii
Copy-Item -LiteralPath $Manifest -Destination (Join-Path $output 'runtime-manifest.json')
return [pscustomobject]@{ RuntimeDirectory = $runtime; Archive = $bundle; Sha256 = $checksum }

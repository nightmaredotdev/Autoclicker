<#
.SYNOPSIS
    Builds the portable Windows release ZIP of Autoclicker.

.DESCRIPTION
    1. Builds the release executable (unless -NoBuild).
    2. Stages autoclicker.exe, README.txt, LICENSE.txt, VERSION.txt and
       THIRD-PARTY-NOTICES.txt (the license texts of every crate compiled into
       the executable, collected from the cargo registry).
    3. Writes target\package\Autoclicker-<version>-windows-x64.zip.

    The ZIP is deterministic: entries are sorted, every entry gets the commit
    timestamp, and text files are written byte-for-byte reproducibly. With the
    same commit and the same executable, the ZIP is identical (the release
    executable itself is reproducible thanks to /Brepro in .cargo\config.toml).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#>
param([switch]$NoBuild)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Invoke-Checked([string]$exe, [string[]]$arguments) {
    $out = & $exe @arguments
    if ($LASTEXITCODE -ne 0) { throw "$exe $($arguments -join ' ') failed ($LASTEXITCODE)" }
    return $out
}

$target = 'x86_64-pc-windows-msvc'
$metadata = (Invoke-Checked 'cargo' @('metadata', '--format-version', '1', '--locked', '--filter-platform', $target)) -join "`n" | ConvertFrom-Json
$app = $metadata.packages | Where-Object { $_.name -eq 'autoclicker' } | Select-Object -First 1
$version = $app.version

if (-not $NoBuild) {
    Invoke-Checked 'cargo' @('build', '--release', '--locked', '-p', 'autoclicker') | Out-Null
}
$exe = Join-Path $root 'target\release\autoclicker.exe'
if (-not (Test-Path $exe)) { throw "missing $exe (build first or omit -NoBuild)" }

$commit = (Invoke-Checked 'git' @('rev-parse', 'HEAD')).Trim()
$commitEpoch = [long](Invoke-Checked 'git' @('log', '-1', '--format=%ct')).Trim()
$timestamp = [DateTimeOffset]::FromUnixTimeSeconds($commitEpoch)
$dirty = (Invoke-Checked 'git' @('status', '--porcelain', '--untracked-files=no')) -ne $null

# ---- third-party notices: crates linked into the executable ----------------
$byId = @{}
foreach ($p in $metadata.packages) { $byId[$p.id] = $p }
$nodes = @{}
foreach ($n in $metadata.resolve.nodes) { $nodes[$n.id] = $n }
$seen = New-Object 'System.Collections.Generic.HashSet[string]'
$queue = New-Object System.Collections.Queue
$queue.Enqueue($app.id)
while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    if (-not $seen.Add($id)) { continue }
    foreach ($dep in $nodes[$id].deps) {
        # Normal (runtime) dependencies only: build tools are not shipped.
        $normal = $dep.dep_kinds | Where-Object { $null -eq $_.kind }
        if ($normal) { $queue.Enqueue($dep.pkg) }
    }
}
$crates = $seen | ForEach-Object { $byId[$_] } |
    Where-Object { -not $_.name.StartsWith('autoclicker') } |
    Sort-Object name, version

$licensePattern = '^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([-._].*)?$'
$apacheText = $null
$missing = 0
$supplemented = 0
$notices = New-Object System.Text.StringBuilder
[void]$notices.Append("Third-party notices for Autoclicker $version`n")
[void]$notices.Append("=" * 60 + "`n`n")
[void]$notices.Append(@"
Slint (https://slint.dev) is used under the Slint Royalty-free Desktop,
Mobile, and Web Applications License v2.0. As that license requires, the
application's About window shows the AboutSlint attribution.

The following crates are used to build autoclicker.exe (runtime libraries
and the procedural macros that generate code for it). For each, its declared
license and the license files shipped in its package are included. Crates that
ship no license file are marked; the standard Apache-2.0 text is appended at
the end, and their sources are linked.

"@.Replace("`r`n", "`n"))
$summary = $crates | ForEach-Object { "  {0} {1} ({2})" -f $_.name, $_.version, $_.license }
[void]$notices.Append(($summary -join "`n") + "`n")
foreach ($c in $crates) {
    $dir = Split-Path -Parent $c.manifest_path
    [void]$notices.Append("`n" + ("-" * 60) + "`n")
    [void]$notices.Append("$($c.name) $($c.version)`nLicense: $($c.license)`n")
    if ($c.repository) { [void]$notices.Append("Source: $($c.repository)`n") }
    $files = @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match $licensePattern } | Sort-Object Name)
    $licensesDir = Join-Path $dir 'LICENSES'
    if (Test-Path -LiteralPath $licensesDir) {
        $extra = @(Get-ChildItem -LiteralPath $licensesDir -File | Sort-Object Name)
        if ($c.name -like 'i-slint-*' -or $c.name -like 'slint*') {
            # Slint is triple-licensed; include only the license used here.
            $extra = @($extra | Where-Object { $_.Name -like '*Royalty-free*' })
        }
        $files += $extra
    }
    if ($null -eq $apacheText) {
        $apache = $files | Where-Object { $_.Name -like 'LICENSE-APACHE*' -or $_.Name -eq 'Apache-2.0.txt' } | Select-Object -First 1
        if ($apache) { $apacheText = [System.IO.File]::ReadAllText($apache.FullName).Replace("`r`n", "`n").TrimEnd() }
    }
    if ($files.Count -eq 0) {
        # Supplementary upstream license files for crates published without one.
        $supplement = Join-Path $root "packaging\licenses\$($c.name)-$($c.version)"
        if (Test-Path -LiteralPath $supplement) {
            $source = Join-Path $supplement 'SOURCE.txt'
            if (Test-Path -LiteralPath $source) {
                $origin = [System.IO.File]::ReadAllText($source).Replace("`r`n", "`n").TrimEnd()
                [void]$notices.Append("`n$origin`n")
            }
            $files = @(Get-ChildItem -LiteralPath $supplement -File | Where-Object { $_.Name -ne 'SOURCE.txt' } | Sort-Object Name)
            $supplemented += 1
        }
    }
    if ($files.Count -eq 0) {
        $missing += 1
        [void]$notices.Append("(No license file is included in this crate's package; see its declared license above.)`n")
    }
    foreach ($f in $files) {
        $text = [System.IO.File]::ReadAllText($f.FullName).Replace("`r`n", "`n").TrimEnd()
        [void]$notices.Append("`n--- $($f.Name) ---`n$text`n")
    }
}

if ($apacheText) {
    [void]$notices.Append("`n" + ("=" * 60) + "`nApache License 2.0 (standard text)`n" + ("=" * 60) + "`n$apacheText`n")
}

# ---- staging ----------------------------------------------------------------
$name = "Autoclicker-$version-windows-x64"
$outDir = Join-Path $root 'target\package'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$zipPath = Join-Path $outDir "$name.zip"

$readme = [System.IO.File]::ReadAllText((Join-Path $root 'packaging\README.txt')).Replace("`r`n", "`n").Replace('{{VERSION}}', $version)
$license = [System.IO.File]::ReadAllText((Join-Path $root 'LICENSE')).Replace("`r`n", "`n")
$versionText = "Autoclicker $version`nCommit: $commit$(if ($dirty) { ' (with uncommitted changes)' })`nCommit date: $($timestamp.ToString('yyyy-MM-dd HH:mm:ss')) UTC`nTarget: $target`n"

function To-Crlf([string]$s) { return $s.Replace("`n", "`r`n") }
$entries = [ordered]@{
    "$name/autoclicker.exe"          = [System.IO.File]::ReadAllBytes($exe)
    "$name/LICENSE.txt"              = $utf8.GetBytes((To-Crlf $license))
    "$name/README.txt"               = $utf8.GetBytes((To-Crlf $readme))
    "$name/THIRD-PARTY-NOTICES.txt"  = $utf8.GetBytes((To-Crlf $notices.ToString()))
    "$name/VERSION.txt"              = $utf8.GetBytes((To-Crlf $versionText))
}

# ---- deterministic ZIP --------------------------------------------------------
Add-Type -AssemblyName System.IO.Compression
if (Test-Path $zipPath) { Remove-Item $zipPath }
$stream = [System.IO.File]::Open($zipPath, [System.IO.FileMode]::CreateNew)
try {
    $zip = New-Object System.IO.Compression.ZipArchive($stream, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($key in ($entries.Keys | Sort-Object)) {
            $entry = $zip.CreateEntry($key, [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = $timestamp
            $w = $entry.Open()
            try { $bytes = $entries[$key]; $w.Write($bytes, 0, $bytes.Length) } finally { $w.Dispose() }
        }
    } finally { $zip.Dispose() }
} finally { $stream.Dispose() }

$hash = (Get-FileHash -Algorithm SHA256 $zipPath).Hash
Write-Output "Created $zipPath"
Write-Output "  crates listed in THIRD-PARTY-NOTICES.txt: $($crates.Count) ($supplemented with license files from packaging\licenses, $missing without a license file)"
Write-Output "  SHA-256: $hash"
if ($dirty) { Write-Output "  note: the working tree has uncommitted changes" }

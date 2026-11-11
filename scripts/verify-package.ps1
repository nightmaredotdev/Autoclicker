<#
.SYNOPSIS
    Verifies the portable release ZIP produced by scripts\package.ps1.

.DESCRIPTION
    Always (no desktop needed, used in CI):
      * extracts the ZIP into a new directory whose path contains spaces;
      * checks the exact file list and VERSION.txt;
      * checks the executable's version information, embedded icon and
        manifest (asInvoker, per-monitor-v2 DPI awareness).
    -Reproducible: packages again and requires an identical SHA-256.
    -Launch (interactive desktop): starts the extracted executable from that
      directory with its own single-instance name and log directory, checks
      that it uses %APPDATA%\Autoclicker for settings and writes nothing next
      to itself, then ends it through the session-end path and requires exit
      code 0. It reads, but does not change, your settings file (except the
      one-time removal of obsolete Start/Stop hotkeys from a pre-1.0 file).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\verify-package.ps1 -Reproducible -Launch
#>
param([string]$ZipPath, [switch]$Reproducible, [switch]$Launch)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Fail([string]$message) { throw "VERIFY FAILED: $message" }
function Ok([string]$message) { Write-Output "  ok  $message" }

$version = ((& cargo metadata --format-version 1 --no-deps --locked) -join "`n" | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq 'autoclicker' } | ForEach-Object { $_.version }
$name = "Autoclicker-$version-windows-x64"
if (-not $ZipPath) { $ZipPath = Join-Path $root "target\package\$name.zip" }
if (-not (Test-Path $ZipPath)) { Fail "missing $ZipPath" }
Write-Output "Verifying $ZipPath"

# ---- extract into a different directory -------------------------------------
$work = Join-Path ([IO.Path]::GetTempPath()) ("autoclicker verify " + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($ZipPath, $work)
    $folder = Join-Path $work $name
    $expected = @('LICENSE.txt', 'README.txt', 'THIRD-PARTY-NOTICES.txt', 'VERSION.txt', 'autoclicker.exe')
    $actual = @(Get-ChildItem -LiteralPath $folder -File | ForEach-Object Name | Sort-Object)
    if ((Compare-Object $expected $actual) -or @(Get-ChildItem -LiteralPath $work).Count -ne 1) {
        Fail "unexpected contents: $($actual -join ', ')"
    }
    Ok "contents: $($actual -join ', ')"

    $versionText = Get-Content -LiteralPath (Join-Path $folder 'VERSION.txt') -Raw
    if (-not $versionText.StartsWith("Autoclicker $version")) { Fail "VERSION.txt: $versionText" }
    Ok "VERSION.txt names version $version"
    $notices = Get-Content -LiteralPath (Join-Path $folder 'THIRD-PARTY-NOTICES.txt') -Raw
    if (-not $notices.Contains('Slint Royalty-free') -or -not $notices.Contains('Apache License')) { Fail 'third-party notices incomplete' }
    Ok "third-party notices include the Slint license and license texts ($([math]::Round($notices.Length / 1MB, 1)) MB)"
    $normalizedNotices = $notices.Replace("`r`n", "`n")
    foreach ($dir in @(Get-ChildItem -LiteralPath (Join-Path $root 'packaging\licenses') -Directory)) {
        foreach ($file in @(Get-ChildItem -LiteralPath $dir.FullName -File | Where-Object { $_.Name -ne 'SOURCE.txt' })) {
            $license = [System.IO.File]::ReadAllText($file.FullName).Replace("`r`n", "`n").TrimEnd()
            $header = "`n$($dir.Name -replace '-(\d[^-]*)$', ' $1')`n"
            if (-not $normalizedNotices.Contains($header)) { Fail "notices lack the section for $($dir.Name)" }
            if (-not $normalizedNotices.Contains("--- $($file.Name) ---`n$license`n")) { Fail "notices lack $($dir.Name)/$($file.Name)" }
            Ok "supplementary license included verbatim: $($dir.Name)/$($file.Name)"
        }
    }

    # ---- executable metadata ----------------------------------------------------
    $exe = Join-Path $folder 'autoclicker.exe'
    $info = (Get-Item -LiteralPath $exe).VersionInfo
    foreach ($pair in @(@('ProductName', 'Autoclicker'), @('FileDescription', 'Autoclicker'),
                        @('OriginalFilename', 'autoclicker.exe'), @('ProductVersion', $version), @('FileVersion', $version))) {
        if ($info.($pair[0]) -ne $pair[1]) { Fail "$($pair[0]) is '$($info.($pair[0]))', expected '$($pair[1])'" }
    }
    Ok "version info: $($info.ProductName) $($info.ProductVersion), '$($info.FileDescription)', $($info.LegalCopyright)"

    Add-Type -AssemblyName System.Drawing
    $icon = [System.Drawing.Icon]::ExtractAssociatedIcon($exe)
    $bmp = $icon.ToBitmap()
    # The blue tile, and the white cursor of the Cadence logo (assets/icon).
    $px = $bmp.GetPixel([int]($bmp.Width * 0.83), [int]($bmp.Height * 0.2))
    if (-not ($px.B -gt 180 -and $px.R -lt 100 -and $px.A -gt 200)) { Fail "embedded icon not found (pixel $px)" }
    $glyph = $bmp.GetPixel([int]($bmp.Width * 0.6875), [int]($bmp.Height * 0.5))
    if (-not ($glyph.R -gt 230 -and $glyph.G -gt 230 -and $glyph.B -gt 230 -and $glyph.A -gt 200)) { Fail "embedded icon is not the Cadence logo (pixel $glyph)" }
    Ok "embedded application icon ($($bmp.Width)x$($bmp.Height), background $($px.R),$($px.G),$($px.B), logo $($glyph.R),$($glyph.G),$($glyph.B))"

    $text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($exe))
    foreach ($needle in @('level="asInvoker"', 'PerMonitorV2', "version=""$version.0""")) {
        if (-not $text.Contains($needle)) { Fail "manifest lacks $needle" }
    }
    Ok 'manifest: asInvoker, per-monitor-v2 DPI awareness, version matches'

    # ---- reproducibility ------------------------------------------------------------
    if ($Reproducible) {
        $first = (Get-FileHash -Algorithm SHA256 $ZipPath).Hash
        $copy = Join-Path $work 'first.zip'
        Copy-Item $ZipPath $copy
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'package.ps1') -NoBuild | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail 'repackaging failed' }
        $second = (Get-FileHash -Algorithm SHA256 $ZipPath).Hash
        if ($first -ne $second) { Fail "ZIP is not reproducible ($first vs $second)" }
        Remove-Item $copy
        Ok "reproducible: packaging again gives the same SHA-256 ($first)"
    }

    # ---- launch from the extracted directory ------------------------------------------
    if ($Launch) {
        Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices; using System.Collections.Generic;
public static class VerifyWin {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern IntPtr SendMessageTimeoutW(IntPtr h, uint m, IntPtr w, IntPtr l, uint f, uint t, out IntPtr r);
  public static IntPtr Find(uint pid, string title, bool visibleOnly) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p != pid) return true;
      var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
      if (sb.ToString().StartsWith(title) && (!visibleOnly || IsWindowVisible(h))) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
  public static bool EndSession(IntPtr h) {
    IntPtr r; if (SendMessageTimeoutW(h, 0x0011, IntPtr.Zero, IntPtr.Zero, 2, 10000, out r) == IntPtr.Zero) return false;
    IntPtr ignored; return SendMessageTimeoutW(h, 0x0016, (IntPtr)1, IntPtr.Zero, 2, 10000, out ignored) != IntPtr.Zero;
  }
}
"@
        $logs = Join-Path $work 'logs'
        $psi = New-Object System.Diagnostics.ProcessStartInfo $exe
        $psi.WorkingDirectory = $folder
        $psi.UseShellExecute = $false
        $psi.EnvironmentVariables['AUTOCLICKER_INSTANCE'] = "AutoclickerVerify.$PID"
        $psi.EnvironmentVariables['AUTOCLICKER_LOG_DIR'] = $logs
        [void]$psi.EnvironmentVariables.Remove('AUTOCLICKER_CONFIG')
        $proc = [System.Diagnostics.Process]::Start($psi)
        try {
            $deadline = (Get-Date).AddSeconds(15)
            while ([VerifyWin]::Find([uint32]$proc.Id, 'Autoclicker', $true) -eq [IntPtr]::Zero) {
                if ($proc.HasExited) { Fail "exited early with code $($proc.ExitCode)" }
                if ((Get-Date) -gt $deadline) { Fail 'main window did not appear' }
                Start-Sleep -Milliseconds 100
            }
            Ok 'starts from the extracted directory and shows its window'

            $log = Get-Content -LiteralPath (Join-Path $logs 'autoclicker.log') -Raw
            $appData = [Environment]::GetFolderPath('ApplicationData')
            $profile = [Environment]::GetFolderPath('UserProfile')
            $expectedConfig = (Join-Path $appData 'Autoclicker\config.toml').Replace($profile, '%USERPROFILE%')
            if (-not $log.Contains("settings file $expectedConfig")) { Fail "settings not under %APPDATA%:`n$log" }
            Ok "settings file: $expectedConfig"

            $events = [VerifyWin]::Find([uint32]$proc.Id, 'Autoclicker system events', $false)
            if (-not [VerifyWin]::EndSession($events)) { Fail 'session-end messages failed' }
            if (-not $proc.WaitForExit(10000)) { Fail 'did not exit after the session ended' }
            if ($proc.ExitCode -ne 0) { Fail "exit code $($proc.ExitCode)" }
            Ok 'clean exit (code 0) through the session-end path'
        } finally {
            if (-not $proc.HasExited) { $proc.Kill() }
        }
        $after = @(Get-ChildItem -LiteralPath $folder -Recurse -Force | ForEach-Object Name | Sort-Object)
        if (Compare-Object $expected $after) { Fail "files were written next to the executable: $($after -join ', ')" }
        Ok 'nothing written to the application directory'
    }
    Write-Output 'Package verified.'
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

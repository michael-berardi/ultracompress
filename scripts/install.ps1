# UltraCompress installer for Windows (PowerShell 5.1 and 7):
#   irm https://github.com/your-github-username/ultracompress/releases/latest/download/install.ps1 | iex
# Downloads the release zip for this machine, checks its SHA-256 before anything is installed, puts
# ultracompress.exe under %LOCALAPPDATA%\UltraCompress and a .cmd shim in its bin folder, and adds that
# bin folder to your USER Path when it is not there. No admin rights, re-running upgrades in place.
# Also runs under PowerShell 7 on Linux for testing: there it installs the Linux release the same way.
# Environment: ULTRACOMPRESS_HOME, ULTRACOMPRESS_BIN_DIR, ULTRACOMPRESS_VERSION (e.g. 0.5.1),
# ULTRACOMPRESS_RELEASE_BASE (download base override, used by tests).
# The @RELEASE_REPO@ token is replaced with the real owner/repo by scripts/build-release-cross.sh.
$ErrorActionPreference = 'Stop'

$Repo = '@RELEASE_REPO@'
# PowerShell 5.1 has no $IsWindows; it only runs on Windows.
$IsWin = if (Test-Path Variable:IsWindows) { $IsWindows } else { $true }
if ($env:ULTRACOMPRESS_RELEASE_BASE) { $Base = $env:ULTRACOMPRESS_RELEASE_BASE }
elseif ($env:ULTRACOMPRESS_VERSION) { $Base = "https://github.com/$Repo/releases/download/v$($env:ULTRACOMPRESS_VERSION.TrimStart('v'))" }
else { $Base = "https://github.com/$Repo/releases/latest/download" }
# The guard matches the shape of the unreplaced token, never the token itself, so stamping cannot rewrite it.
if ($Base -like '*github.com/@*@/*') {
  throw 'ultracompress install: this install.ps1 is a source template, not a release asset; use the install.ps1 attached to a GitHub release, or set ULTRACOMPRESS_RELEASE_BASE to a release download base.'
}
if ($env:ULTRACOMPRESS_HOME) { $HomeDir = $env:ULTRACOMPRESS_HOME }
elseif ($IsWin) { $HomeDir = Join-Path $env:LOCALAPPDATA 'UltraCompress' }
else { $HomeDir = Join-Path $HOME '.ultracompress' }
if ($env:ULTRACOMPRESS_BIN_DIR) { $BinDir = $env:ULTRACOMPRESS_BIN_DIR }
else { $BinDir = Join-Path $HomeDir 'bin' }

if ($IsWin) {
  # PowerShell 5.1 may default to TLS 1.0; GitHub needs TLS 1.2.
  try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch { }
  $Asset = 'ultracompress-windows-x86_64.zip'
  $ExeName = 'ultracompress.exe'
} else {
  $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
  if ($arch -eq 'X64') { $Asset = 'ultracompress-linux-x86_64.tar.gz' }
  elseif ($arch -eq 'Arm64') { $Asset = 'ultracompress-linux-arm64.tar.gz' }
  else { throw "ultracompress install: no Linux release for CPU $arch; build from source with Rust 1.85+: cargo build --locked --release" }
  $ExeName = 'ultracompress'
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('ultracompress-install-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "Downloading UltraCompress ($Asset)..."
  $archive = Join-Path $tmp $Asset
  Invoke-WebRequest "$Base/$Asset" -OutFile $archive -UseBasicParsing
  $sumsFile = Join-Path $tmp 'SHA256SUMS'
  Invoke-WebRequest "$Base/SHA256SUMS" -OutFile $sumsFile -UseBasicParsing
  $line = Get-Content $sumsFile | Where-Object { $_ -match ('\s' + [regex]::Escape($Asset) + '$') } | Select-Object -First 1
  $want = if ($line) { ($line -split '\s+')[0].ToLower() } else { '' }
  if (-not $want) { throw "ultracompress install: no checksum for $Asset in SHA256SUMS; nothing was installed." }
  $got = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
  if ($got -ne $want) { throw "ultracompress install: checksum mismatch for $Asset; nothing was installed." }

  $x = Join-Path $tmp 'x'
  New-Item -ItemType Directory -Path $x | Out-Null
  if ($Asset -like '*.zip') {
    Expand-Archive -Path $archive -DestinationPath $x -Force
  } else {
    & tar -xzf $archive -C $x
    if ($LASTEXITCODE -ne 0) { throw "ultracompress install: cannot unpack $Asset (tar exit $LASTEXITCODE)." }
  }
  $staged = Join-Path $x $ExeName
  if (-not (Test-Path $staged)) { throw 'ultracompress install: the download is not an UltraCompress release (no binary inside).' }

  # Prove the staged binary runs here before anything is installed.
  $ver = & $staged --version 2>&1
  if ($LASTEXITCODE -ne 0) { throw "ultracompress install: the downloaded binary does not run on this machine: $ver" }
  $ver = [string]$ver
  if ($ver -notmatch '^ultracompress \S+') { throw "ultracompress install: the downloaded binary reports '$ver', not a version; nothing was installed." }
  $Version = $ver.Substring('ultracompress '.Length).Trim()

  New-Item -ItemType Directory -Path $HomeDir -Force | Out-Null
  New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
  Move-Item -Force -Path $staged -Destination (Join-Path $HomeDir $ExeName)
  foreach ($f in 'LICENSE', 'README.md') {
    $p = Join-Path $x $f
    if (Test-Path $p) { Copy-Item $p (Join-Path $HomeDir $f) -Force }
  }
  $installed = Join-Path $HomeDir $ExeName
  if ($IsWin) {
    $shim = Join-Path $BinDir 'ultracompress.cmd'
    Set-Content -Path $shim -Value "@echo off`r`n`"%~dp0..\$ExeName`" %*" -Encoding Ascii
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    if (($userPath -split ';') -notcontains $BinDir) {
      $newPath = if ($userPath) { "$userPath;$BinDir" } else { $BinDir }
      [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
      Write-Host "Added $BinDir to your USER Path; open a new terminal to pick it up."
    }
  } else {
    $shim = Join-Path $BinDir 'ultracompress'
    Set-Content -Path $shim -Value "#!/bin/sh`nexec `"$installed`" `"`$@`"`n" -Encoding Ascii
    & chmod 755 $shim
    if ($LASTEXITCODE -ne 0) { throw 'ultracompress install: cannot make the shim executable (chmod failed).' }
    if (($env:PATH -split ':') -notcontains $BinDir) { Write-Host "Add $BinDir to your PATH:  export PATH=`"$BinDir`:`$PATH`"" }
  }

  $check = & $shim --version 2>&1
  if ($LASTEXITCODE -ne 0) { throw "ultracompress install: the installed command does not run: $check" }
  Write-Host "UltraCompress $Version installed ($shim)."
  Write-Host 'Next: ultracompress plan --session /path/to/session.jsonl'
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
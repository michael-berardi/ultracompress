# UltraCompress installer for Windows (PowerShell 5.1 and 7):
#   irm https://github.com/your-github-username/ultracompress/releases/latest/download/install.ps1 | iex
# Downloads the release zip for this machine, checks its SHA-256 before anything is installed, puts
# ultracompress.exe under %LOCALAPPDATA%\UltraCompress and a .cmd shim in its bin folder, and adds that
# bin folder to your USER Path when it is not there. No admin rights, re-running upgrades in place.
# Also runs under PowerShell 7 on Linux for testing: there it installs the Linux release the same way.
# Environment: ULTRACOMPRESS_HOME, ULTRACOMPRESS_BIN_DIR, ULTRACOMPRESS_VERSION (e.g. 0.5.1),
# ULTRACOMPRESS_RELEASE_BASE (download base override, used by tests).
# The @RELEASE_REPO@ token is replaced with the real owner/repo by scripts/build-release-cross.sh.
# Everything runs inside a function that is removed afterwards, so nothing (preferences, variables) leaks into your session.
function Install-UltraCompress {
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
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
  # Absolute, native-separator paths, whatever the override looked like (relative, or written with /).
  $HomeDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($HomeDir)
  if ($env:ULTRACOMPRESS_BIN_DIR) { $BinDir = $env:ULTRACOMPRESS_BIN_DIR }
  else { $BinDir = Join-Path $HomeDir 'bin' }
  $BinDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($BinDir)

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

  #region shim-helpers
  $ShimMarker = 'ultracompress-installer-shim'
  # True when this installer wrote the shim (marker line) or 0.6.1 did (its exact two-line shape).
  function Test-OwnShim([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    if ((Get-Item -LiteralPath $Path -Force).LinkType) { return $false }
    $lines = @(Get-Content -LiteralPath $Path -ErrorAction SilentlyContinue | Where-Object { $_.Trim() -ne '' })
    if ($lines -contains "# $ShimMarker" -or $lines -contains "rem $ShimMarker") { return $true }
    if ($lines.Count -ne 2) { return $false }
    if ($lines[0] -eq '#!/bin/sh' -and $lines[1] -match '^exec ".*/ultracompress" "\$@"$') { return $true }
    if ($lines[0] -eq '@echo off' -and $lines[1] -eq '"%~dp0..\ultracompress.exe" %*') { return $true }
    return $false
  }
  # The .cmd shim text and the encoding to write it with. Under the tool's own home it is %~dp0-relative (pure ASCII);
  # for a BIN_DIR override the absolute path is written in the OEM code page cmd.exe reads, never as ASCII.
  function Get-CmdShimText([string]$Exe, [string]$HomeDir, [string]$BinDir) {
    $own = $HomeDir.TrimEnd('\', '/') + '\bin'
    if ($BinDir.TrimEnd('\', '/') -eq $own) {
      $target = '%~dp0..\' + (Split-Path -Leaf $Exe)
      $enc = [Text.Encoding]::ASCII
    } else {
      $target = $Exe.Replace('%', '%%')
      $enc = [Text.Encoding]::GetEncoding([Globalization.CultureInfo]::CurrentCulture.TextInfo.OEMCodePage)
    }
    @{ Text = "@echo off`r`nrem $ShimMarker`r`n`"$target`" %*`r`n"; Encoding = $enc }
  }
  # Writes a shim; refuses text the encoding cannot represent instead of silently writing '?'.
  function Write-ShimFile([string]$Path, [string]$Text, [Text.Encoding]$Encoding) {
    if ($Encoding.GetString($Encoding.GetBytes($Text)) -ne $Text) {
      throw "ultracompress install: the folder name has characters the console code page cannot hold; set ULTRACOMPRESS_BIN_DIR and ULTRACOMPRESS_HOME to plain ASCII folders. Nothing was changed in your PATH."
    }
    [IO.File]::WriteAllText($Path, $Text, $Encoding)
  }
  #endregion
  #region path-helpers
  # True when $Entries (the raw, unexpanded user Path) already holds $Dir, literally or after expanding %VARS%,
  # ignoring case and a trailing backslash.
  function Test-PathHasEntry([string]$Entries, [string]$Dir) {
    $want = $Dir.TrimEnd('\')
    foreach ($e in ($Entries -split ';')) {
      if (-not $e) { continue }
      if ($e.TrimEnd('\') -ieq $want) { return $true }
      if ([Environment]::ExpandEnvironmentVariables($e).TrimEnd('\') -ieq $want) { return $true }
    }
    return $false
  }
  #endregion

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
    # Windows PowerShell 5.1 turns the first stderr line of a captured native call into a terminating error under 'Stop'.
    $ErrorActionPreference = 'Continue'
    $ver = & $staged --version 2>&1
    $verExit = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($verExit -ne 0) { throw "ultracompress install: the downloaded binary does not run on this machine: $ver" }
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
    if ($IsWin) { $shim = Join-Path $BinDir 'ultracompress.cmd' } else { $shim = Join-Path $BinDir 'ultracompress' }
    $writeShim = (-not (Test-Path -LiteralPath $shim)) -or (Test-OwnShim $shim)
    if ($writeShim) {
      if ($IsWin) {
        $shimText = Get-CmdShimText $installed $HomeDir $BinDir
        Write-ShimFile $shim $shimText.Text $shimText.Encoding
        # Edit the registry value directly: the .NET Get/SetEnvironmentVariable pair expands %VARS% and stores REG_SZ.
        $envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        try {
          $userPath = [string]$envKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
          if (-not (Test-PathHasEntry $userPath $BinDir)) {
            $newPath = if ($userPath) { $userPath.TrimEnd(';') + ';' + $BinDir } else { $BinDir }
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            if ($null -ne $envKey.GetValue('Path', $null)) { $kind = $envKey.GetValueKind('Path') }
            $envKey.SetValue('Path', $newPath, $kind)
            # Setting and clearing a user variable broadcasts the change so new terminals pick the Path up.
            [Environment]::SetEnvironmentVariable('ULTRACOMPRESS_PATH_REFRESH', '1', 'User')
            [Environment]::SetEnvironmentVariable('ULTRACOMPRESS_PATH_REFRESH', $null, 'User')
            Write-Host "Added $BinDir to your USER Path; open a new terminal to pick it up."
          }
        } finally { if ($envKey) { $envKey.Close() } }
      } else {
        Write-ShimFile $shim "#!/bin/sh`n# $ShimMarker`nexec `"$installed`" `"`$@`"`n" (New-Object Text.UTF8Encoding($false))
        & chmod 755 $shim
        if ($LASTEXITCODE -ne 0) { throw 'ultracompress install: cannot make the shim executable (chmod failed).' }
        if (($env:PATH -split ':') -notcontains $BinDir) { Write-Host "Add $BinDir to your PATH:  export PATH=`"$BinDir`:`$PATH`"" }
      }
    } else {
      Write-Host "Left $shim alone: it was not written by this installer. Run $installed, or remove that file and re-run."
      $shim = $installed
    }

    $ErrorActionPreference = 'Continue'
    $check = & $shim --version 2>&1
    $checkExit = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($checkExit -ne 0) { throw "ultracompress install: the installed command does not run: $check" }
    Write-Host "UltraCompress $Version installed ($shim)."
    Write-Host 'Next: ultracompress plan --session /path/to/session.jsonl'
  } finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
  }
}
try { Install-UltraCompress } finally { Remove-Item Function:\Install-UltraCompress -ErrorAction SilentlyContinue }

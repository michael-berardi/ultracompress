#!/usr/bin/env bash
# Installer tests against a local release base served by a short-lived http server.
# Prerequisites: scripts/build-release-cross.sh has produced dist/release (Linux/Windows assets and
# the release installers). On macOS the test builds an UNSIGNED universal zip locally for the install
# test (the signed flow in scripts/release.sh is never run); set ULTRACOMPRESS_TEST_MAC_ZIP to use an
# already published zip instead. Linux and PowerShell 7 checks run in Docker containers.
# Usage: scripts/test-install.sh
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(pwd -P)
PORT="${ULTRACOMPRESS_TEST_PORT:-18796}"
REL="$root/dist/release"
say() { printf '%s\n' "$*"; }
die() { printf 'test-install: %s\n' "$*" >&2; exit 1; }

for f in ultracompress-linux-arm64.tar.gz ultracompress-linux-x86_64.tar.gz ultracompress-windows-x86_64.zip install.sh install.ps1; do
  [ -f "$REL/$f" ] || die "missing $REL/$f; run scripts/build-release-cross.sh first"
done
command -v python3 >/dev/null 2>&1 || die 'needs python3 to serve the local release base'
command -v docker >/dev/null 2>&1 || die 'needs Docker for the Linux and PowerShell tests'
if nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1; then die "port $PORT is already in use; set ULTRACOMPRESS_TEST_PORT"; fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/ultracompress-test.XXXXXX")
BASE="$WORK/base"
mkdir -p "$BASE"
SRV=""
cleanup() {
  if [ -n "$SRV" ]; then kill "$SRV" 2>/dev/null || true; wait "$SRV" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cp "$REL/ultracompress-linux-arm64.tar.gz" "$REL/ultracompress-linux-x86_64.tar.gz" \
   "$REL/ultracompress-windows-x86_64.zip" "$REL/install.sh" "$REL/install.ps1" "$BASE/"

# macOS test asset: a locally built universal zip (unsigned; test-only, never published).
if [ -n "${ULTRACOMPRESS_TEST_MAC_ZIP:-}" ]; then
  cp "$ULTRACOMPRESS_TEST_MAC_ZIP" "$BASE/UltraCompress-macos-universal.zip"
elif [ -f "$REL/UltraCompress-macos-universal.zip" ]; then
  cp "$REL/UltraCompress-macos-universal.zip" "$BASE/UltraCompress-macos-universal.zip"
else
  [ "$(uname -s)" = Darwin ] || die 'no macOS zip available; set ULTRACOMPRESS_TEST_MAC_ZIP'
  say 'Building an unsigned universal macOS zip for the install test...'
  cargo build --locked --release -p ultracompress --target aarch64-apple-darwin --target x86_64-apple-darwin
  mkdir -p "$WORK/cli/cli"
  lipo -create -output "$WORK/cli/cli/ultracompress" \
    target/aarch64-apple-darwin/release/ultracompress target/x86_64-apple-darwin/release/ultracompress
  cp LICENSE README.md "$WORK/cli/cli/"
  (cd "$WORK/cli" && ditto -c -k --norsrc --keepParent cli "$BASE/UltraCompress-macos-universal.zip")
fi

( cd "$BASE" && find . -maxdepth 1 -type f | LC_ALL=C sort | sed 's|^\./||' | while read -r f; do
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$f"; else shasum -a 256 "$f"; fi
  done > SHA256SUMS )

python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$BASE" >/dev/null 2>&1 &
SRV=$!
LOCAL_BASE="http://127.0.0.1:$PORT"
DOCKER_BASE="http://host.docker.internal:$PORT"
for _ in 1 2 3 4 5 6 7 8 9 10; do
  curl -fsS "$LOCAL_BASE/SHA256SUMS" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "$LOCAL_BASE/SHA256SUMS" >/dev/null || die "the local release server did not start on port $PORT"

if [ "$(uname -s)" = Darwin ]; then
  say '== macOS: install, version, upgrade =='
  H1="$WORK/home1"; mkdir -p "$H1"
  HOME="$H1" ULTRACOMPRESS_RELEASE_BASE="$LOCAL_BASE" sh "$BASE/install.sh"
  v=$("$H1/.local/bin/ultracompress" --version)
  case "$v" in "ultracompress "*) ;; *) die "installed command printed '$v'" ;; esac
  HOME="$H1" ULTRACOMPRESS_RELEASE_BASE="$LOCAL_BASE" sh "$BASE/install.sh"
  say "macOS install and upgrade OK ($v)"

  say '== macOS: corrupted archive refused =='
  cp "$BASE/UltraCompress-macos-universal.zip" "$WORK/mac.zip.bak"
  printf 'corruption' >> "$BASE/UltraCompress-macos-universal.zip"
  H2="$WORK/home2"; mkdir -p "$H2"
  if err=$(HOME="$H2" ULTRACOMPRESS_RELEASE_BASE="$LOCAL_BASE" sh "$BASE/install.sh" 2>&1); then
    mv "$WORK/mac.zip.bak" "$BASE/UltraCompress-macos-universal.zip"
    die 'a corrupted archive was accepted'
  fi
  mv "$WORK/mac.zip.bak" "$BASE/UltraCompress-macos-universal.zip"
  case "$err" in *'checksum mismatch'*) ;; *) die "wrong error for a corrupted archive: $err" ;; esac
  [ ! -e "$H2/.ultracompress/ultracompress" ] && [ ! -e "$H2/.local/bin/ultracompress" ] || die 'a corrupted install left files behind'
  say 'macOS corrupted archive refused, nothing installed'

  say '== macOS: missing prerequisite named =='
  mkdir -p "$WORK/emptybin"
  if err=$(PATH="$WORK/emptybin" /bin/sh "$BASE/install.sh" 2>&1); then die 'missing curl/wget was not detected'; fi
  case "$err" in *'needs curl or wget'*) ;; *) die "wrong error for missing prerequisites: $err" ;; esac
  say 'macOS missing prerequisite named'
fi

say '== Linux (ubuntu:24.04, arm64): install, version, upgrade, corrupted archive =='
docker run --rm --name ultracompress-test-linux -v "$BASE":/rel ubuntu:24.04 bash -euo pipefail -c "
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq && apt-get install -y -qq curl ca-certificates >/dev/null
  export ULTRACOMPRESS_RELEASE_BASE='$DOCKER_BASE'
  export HOME=/tmp/h1
  sh /rel/install.sh
  /tmp/h1/.local/bin/ultracompress --version
  sh /rel/install.sh
  printf corruption >> /rel/ultracompress-linux-arm64.tar.gz
  export HOME=/tmp/h2
  if sh /rel/install.sh 2>/tmp/err; then echo 'a corrupted archive was accepted' >&2; exit 1; fi
  grep -q 'checksum mismatch' /tmp/err || { cat /tmp/err >&2; exit 1; }
  [ ! -e /tmp/h2/.ultracompress/ultracompress ] || { echo 'corrupted install left files behind' >&2; exit 1; }
  echo 'Linux install, upgrade and corruption checks OK'
"
# The container appended to the served archive; restore it from the pristine release copy.
cp "$REL/ultracompress-linux-arm64.tar.gz" "$BASE/ultracompress-linux-arm64.tar.gz"

say '== PowerShell 7 (Linux container): install, version, upgrade =='
docker run --rm --name ultracompress-test-pwsh -v "$BASE":/rel:ro mcr.microsoft.com/powershell \
  pwsh -NoProfile -Command "
    \$ErrorActionPreference = 'Stop'
    \$env:ULTRACOMPRESS_RELEASE_BASE = '$DOCKER_BASE'
    \$env:HOME = '/tmp/h1'
    & /rel/install.ps1
    & /tmp/h1/.ultracompress/bin/ultracompress --version
    if (\$LASTEXITCODE -ne 0) { exit 1 }
    & /rel/install.ps1
    Write-Host 'pwsh install and upgrade OK'
    try {
      Install-Module PSScriptAnalyzer -Force -Scope CurrentUser -ErrorAction Stop
      \$r = Invoke-ScriptAnalyzer -Path /rel/install.ps1
      if (\$r) { \$r | Format-Table -AutoSize | Out-String | Write-Host }
      \$errors = @(\$r | Where-Object Severity -eq 'Error')
      if (\$errors.Count -gt 0) { exit 1 }
      Write-Host 'PSScriptAnalyzer: no errors'
    } catch { Write-Host \"PSScriptAnalyzer skipped: \$_\" }
  "

say '== PowerShell 7 (Linux container): corrupted archive refused =='
printf 'corruption' >> "$BASE/ultracompress-windows-x86_64.zip"
if docker run --rm --name ultracompress-test-pwsh-bad -v "$BASE":/rel:ro mcr.microsoft.com/powershell \
  pwsh -NoProfile -Command "
    \$env:ULTRACOMPRESS_RELEASE_BASE = '$DOCKER_BASE'
    \$env:HOME = '/tmp/h2'
    try { & /rel/install.ps1 } catch { Write-Host \$_.Exception.Message; if (\$_.Exception.Message -like '*checksum mismatch*') { exit 0 } ; exit 1 }
    exit 1
  "; then
  say 'pwsh corrupted archive refused'
else
  cp "$REL/ultracompress-windows-x86_64.zip" "$BASE/ultracompress-windows-x86_64.zip"
  die 'pwsh did not refuse the corrupted archive with a checksum error'
fi
cp "$REL/ultracompress-windows-x86_64.zip" "$BASE/ultracompress-windows-x86_64.zip"

kill "$SRV" 2>/dev/null || true
wait "$SRV" 2>/dev/null || true
SRV=""
for _ in 1 2 3 4 5; do
  nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1 || break
  sleep 1
done
if nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1; then die "port $PORT was not released"; fi
say "port $PORT released"
say 'ALL INSTALLER TESTS PASSED'
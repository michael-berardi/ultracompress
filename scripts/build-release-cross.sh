#!/usr/bin/env bash
# Builds the Linux and Windows release assets into dist/release (default), next to the signed macOS
# assets that scripts/release.sh produces. This script never signs, notarizes or touches the macOS flow.
#   ultracompress-linux-arm64.tar.gz    static musl build, native arm64 container
#   ultracompress-linux-x86_64.tar.gz   static musl build, cross-linked with rust-lld (no emulation)
#   ultracompress-windows-x86_64.zip    x86_64-pc-windows-gnu build, mingw-w64 linker
#   install.sh, install.ps1             the installers, with @RELEASE_REPO@ replaced by origin's owner/repo
#   SHA256SUMS                          recomputed over every asset in the output folder except itself
# It also runs `cargo test` in the same container. Usage: scripts/build-release-cross.sh [out-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo 'build-release-cross: the working tree has changes; build from a clean checkout' >&2; exit 1; }
OUT="${1:-dist/release}"
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd -P)
remote=$(git remote get-url origin)
repo=$(printf '%s\n' "$remote" | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')
case "$repo" in */*) ;; *) echo "build-release-cross: cannot read owner/repo from origin remote '$remote'" >&2; exit 1 ;; esac
command -v docker >/dev/null 2>&1 || { echo 'build-release-cross: needs Docker (https://docs.docker.com/get-docker/)' >&2; exit 1; }

# One native arm64 container builds all three targets; x86_64 code is cross-compiled, never emulated.
# The source goes in as a tar of tracked files from HEAD (works from a linked worktree too).
git archive --format=tar HEAD | docker run -i --rm --name ultracompress-cross --platform linux/arm64 \
  -v "$OUT":/out -v ultracompress-cargo-registry:/usr/local/cargo/registry \
  -v ultracompress-cargo-target:/work/target \
  rust:1-bookworm bash -euo pipefail -c '
    mkdir -p /work
    tar -x -C /work
    cd /work
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends gcc-mingw-w64-x86-64 zip file >/dev/null
    rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl x86_64-pc-windows-gnu
    host=$(rustc -vV | sed -n "s/^host: //p")
    LLD="$(rustc --print sysroot)/lib/rustlib/$host/bin/rust-lld"
    [ -x "$LLD" ] || { echo "build-release-cross: rust-lld not found at $LLD" >&2; exit 1; }
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$LLD"
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LLD"
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
    for t in aarch64-unknown-linux-musl x86_64-unknown-linux-musl x86_64-pc-windows-gnu; do
      cargo build --locked --release -p ultracompress --target "$t"
    done
    cargo test --locked --workspace

    pack() { # pack <stage-dir> <binary-source> <binary-name>
      mkdir -p "$1"
      install -m 755 "$2" "$1/$3"
      cp LICENSE README.md "$1/"
      touch -d @0 "$1"/*
    }
    pack /stage-linux-arm64 target/aarch64-unknown-linux-musl/release/ultracompress ultracompress
    pack /stage-linux-x86_64 target/x86_64-unknown-linux-musl/release/ultracompress ultracompress
    pack /stage-windows-x86_64 target/x86_64-pc-windows-gnu/release/ultracompress.exe ultracompress.exe
    file /stage-linux-arm64/ultracompress /stage-linux-x86_64/ultracompress /stage-windows-x86_64/ultracompress.exe
    # musl targets are fully static; with rust-lld the x86_64 build is a static PIE (no dynamic loader either).
    file /stage-linux-arm64/ultracompress | grep -Eq "statically linked|static-pie linked" || { echo "build-release-cross: arm64 Linux binary is not statically linked" >&2; exit 1; }
    file /stage-linux-x86_64/ultracompress | grep -Eq "statically linked|static-pie linked" || { echo "build-release-cross: x86_64 Linux binary is not statically linked" >&2; exit 1; }
    file /stage-windows-x86_64/ultracompress.exe | grep -q "PE32+" || { echo "build-release-cross: Windows binary is not a PE32+ executable" >&2; exit 1; }
    # Deterministic archives: sorted names, fixed owner, epoch timestamps, no gzip timestamp.
    for a in arm64 x86_64; do
      tar -C "/stage-linux-$a" --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 \
        -cf - ultracompress LICENSE README.md | gzip -n > "/out/ultracompress-linux-$a.tar.gz"
    done
    (cd /stage-windows-x86_64 && zip -q -X /out/ultracompress-windows-x86_64.zip ultracompress.exe LICENSE README.md)
  '

# Ship the installers with the real release address (the source tree stays handle-neutral).
sed "s|@RELEASE_REPO@|$repo|g" scripts/install.sh > "$OUT/install.sh"
sed "s|@RELEASE_REPO@|$repo|g" scripts/install.ps1 > "$OUT/install.ps1"
chmod 644 "$OUT/install.sh" "$OUT/install.ps1"
if grep -q '@RELEASE_REPO@' "$OUT/install.sh" "$OUT/install.ps1"; then
  echo 'build-release-cross: the release installers still contain the @RELEASE_REPO@ token' >&2; exit 1
fi
# Refresh checksums over every asset present (including any macOS assets), except SHA256SUMS itself.
( cd "$OUT" && files=$(find . -maxdepth 1 -type f ! -name SHA256SUMS | LC_ALL=C sort | sed 's|^\./||')
  printf '%s\n' "$files" | while read -r f; do
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$f"; else shasum -a 256 "$f"; fi
  done > SHA256SUMS )
echo "Release assets in $OUT:"
ls -l "$OUT"
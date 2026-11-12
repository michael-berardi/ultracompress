#!/bin/sh
# UltraCompress installer for macOS and Linux:
#   curl -fsSL https://github.com/your-github-username/ultracompress/releases/latest/download/install.sh | sh
# Downloads the release binary for this machine (macOS: the signed universal zip; Linux: a static
# musl tarball for this CPU), checks its SHA-256 before anything is installed, and puts `ultracompress`
# on your PATH. No sudo, no shell-profile edits; re-running upgrades in place.
# Layout: $ULTRACOMPRESS_HOME/ultracompress plus a shim in $ULTRACOMPRESS_BIN_DIR.
# Environment: ULTRACOMPRESS_HOME (default ~/.ultracompress), ULTRACOMPRESS_BIN_DIR (default ~/.local/bin),
# ULTRACOMPRESS_VERSION (default: latest, e.g. 0.5.1), ULTRACOMPRESS_RELEASE_BASE (download base override, used by tests).
# The @RELEASE_REPO@ token below is replaced with the real owner/repo by scripts/build-release-cross.sh
# when the release assets are built; a source checkout copy needs ULTRACOMPRESS_RELEASE_BASE.
set -eu

REPO="@RELEASE_REPO@"
HOME_DIR="${ULTRACOMPRESS_HOME:-$HOME/.ultracompress}"
case "$HOME_DIR" in /*) ;; *) HOME_DIR="$(pwd)/$HOME_DIR" ;; esac
BIN_DIR="${ULTRACOMPRESS_BIN_DIR:-$HOME/.local/bin}"
case "$BIN_DIR" in /*) ;; *) BIN_DIR="$(pwd)/$BIN_DIR" ;; esac
if [ -n "${ULTRACOMPRESS_RELEASE_BASE:-}" ]; then BASE="$ULTRACOMPRESS_RELEASE_BASE"
elif [ -n "${ULTRACOMPRESS_VERSION:-}" ]; then BASE="https://github.com/$REPO/releases/download/v${ULTRACOMPRESS_VERSION#v}"
else BASE="https://github.com/$REPO/releases/latest/download"; fi

say() { printf '%s\n' "$*"; }
die() { printf 'ultracompress install: %s\n' "$*" >&2; exit 1; }

if command -v curl >/dev/null 2>&1; then fetch() { curl -fsSL --retry 3 -o "$2" "$1" || die "download failed: $1"; }
elif command -v wget >/dev/null 2>&1; then fetch() { wget -q -O "$2" "$1" || die "download failed: $1"; }
else die "needs curl or wget to download the release; install one first (macOS: brew install curl; Debian/Ubuntu: apt-get install curl)"; fi
command -v tar >/dev/null 2>&1 || die "needs tar to unpack the release; install it first (Debian/Ubuntu: apt-get install tar)"
if command -v sha256sum >/dev/null 2>&1; then sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die "needs sha256sum or shasum to verify the download; install coreutils (Debian/Ubuntu: apt-get install coreutils) or perl (macOS ships shasum)"; fi

main() {
# The guard matches the shape of the unreplaced token, never the token itself, so stamping cannot rewrite it.
case "$BASE" in *github.com/@*@/*) die "this install.sh is a source template, not a release asset; use the install.sh attached to a GitHub release, or set ULTRACOMPRESS_RELEASE_BASE to a release download base" ;; esac
case "$(uname -s)" in
  Darwin) ASSET="UltraCompress-macos-universal.zip" ;;
  Linux)
    case "$(uname -m)" in
      x86_64|amd64) ASSET="ultracompress-linux-x86_64.tar.gz" ;;
      aarch64|arm64) ASSET="ultracompress-linux-arm64.tar.gz" ;;
      *) die "no Linux release for CPU $(uname -m); build from source with Rust 1.85+: cargo build --locked --release" ;;
    esac ;;
  *) die "unsupported system $(uname -s); on Windows use install.ps1" ;;
esac

TMP=$(mktemp -d "${TMPDIR:-/tmp}/ultracompress-install.XXXXXX") || die "cannot create a temporary folder"
trap 'rm -rf "$TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# expect <file> <name> <sums-file>: the file's SHA-256 must equal the line for <name> in the sums file.
expect() {
  want=$(awk -v n="$2" '{ f=$2; sub(/^\*/, "", f); if (f == n) print $1 }' "$3")
  [ -n "$want" ] || die "no checksum for $2 in SHA256SUMS; nothing was installed"
  [ "$(sha256 "$1")" = "$want" ] || die "checksum mismatch for $2; nothing was installed"
}

say "Downloading UltraCompress ($ASSET)..."
fetch "$BASE/$ASSET" "$TMP/$ASSET"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"
expect "$TMP/$ASSET" "$ASSET" "$TMP/SHA256SUMS"
mkdir "$TMP/x"
# macOS releases are zipped; macOS tar (bsdtar) unpacks zip and tar.gz alike.
tar -xf "$TMP/$ASSET" -C "$TMP/x" || die "cannot unpack $ASSET (on macOS the system tar reads the release zip)"
if [ -f "$TMP/x/cli/ultracompress" ]; then SRC="$TMP/x/cli"; else SRC="$TMP/x"; fi
[ -f "$SRC/ultracompress" ] || die "the download is not an UltraCompress release (no ultracompress binary inside)"

# Prove the staged binary runs here before anything is installed.
got=$("$SRC/ultracompress" --version 2>&1) || die "the downloaded binary does not run on this machine: $got"
case "$got" in "ultracompress "*) ;; *) die "the downloaded binary reports '$got', not a version; nothing was installed" ;; esac
VERSION=${got#ultracompress }

mkdir -p "$HOME_DIR" "$BIN_DIR"
# Stage inside the destination folder so the final move is an atomic same-filesystem rename.
cp "$SRC/ultracompress" "$HOME_DIR/.ultracompress.new.$$"
chmod 755 "$HOME_DIR/.ultracompress.new.$$"
mv -f "$HOME_DIR/.ultracompress.new.$$" "$HOME_DIR/ultracompress"
for f in LICENSE README.md; do
  [ -f "$SRC/$f" ] && cp "$SRC/$f" "$HOME_DIR/$f"
done
cat > "$BIN_DIR/.ultracompress.shim.$$" <<SHIM
#!/bin/sh
exec "$HOME_DIR/ultracompress" "\$@"
SHIM
chmod 755 "$BIN_DIR/.ultracompress.shim.$$"
mv -f "$BIN_DIR/.ultracompress.shim.$$" "$BIN_DIR/ultracompress"

got=$("$BIN_DIR/ultracompress" --version 2>&1) || die "the installed command does not run: $got"
say "UltraCompress $VERSION installed ($BIN_DIR/ultracompress)."
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) say "Add $BIN_DIR to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;; esac
say "Next: ultracompress plan --session /path/to/session.jsonl"
}

main "$@"
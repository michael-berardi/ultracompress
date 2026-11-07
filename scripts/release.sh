#!/usr/bin/env bash
set -euo pipefail

# Run from a clean, tagged checkout. Credentials live only in the environment/keychain.
: "${ULTRACOMPRESS_SIGNING_IDENTITY:?error: ULTRACOMPRESS_SIGNING_IDENTITY is required}"
: "${ULTRACOMPRESS_INSTALLER_IDENTITY:?error: ULTRACOMPRESS_INSTALLER_IDENTITY is required}"
: "${NOTARY_PROFILE:?error: NOTARY_PROFILE is required}"
# Keep the installer identifier of earlier releases so pkg upgrades replace the same receipt.
: "${ULTRACOMPRESS_PKG_IDENTIFIER:?error: ULTRACOMPRESS_PKG_IDENTIFIER is required (reuse the identifier of the previous release)}"
root=$(git rev-parse --show-toplevel)
cd "$root"
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
  echo 'error: dirty tree; release requires a clean checkout' >&2; exit 1
fi
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
if ! git rev-parse -q --verify "refs/tags/v${version}^{commit}" >/dev/null || [[ $(git rev-parse HEAD) != "$(git rev-parse "refs/tags/v${version}^{commit}")" ]]; then
  echo "error: HEAD is not the v${version} tag" >&2; exit 1
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
  echo 'error: release requires macOS arm64' >&2; exit 1
fi
out=${ULTRACOMPRESS_OUT:-./dist/release}
mkdir -p "$out"
out=$(cd "$out" && pwd -P)
# Refuse stale results rather than mixing artifacts from different runs.
if [[ -n $(ls -A "$out") ]]; then echo 'error: output directory must be empty' >&2; exit 1; fi
work=$(mktemp -d "${TMPDIR:-/tmp}/ultracompress-release.XXXXXXXX")
trap 'rm -rf "$work"' EXIT
cc_version=$(python3 -c 'import json; print(json.load(open("claude-code/.claude-plugin/plugin.json"))["version"])')
pi_version=$(python3 -c 'import json; print(json.load(open("extension/package.json"))["version"])')
hermes_version=$(python3 -c 'import re; print(re.search(r"(?m)^version:\s*[\x27\x22]?([^\s\x27\x22]+)", open("hermes/ultracompress/plugin.yaml").read()).group(1))')
for v in "$version" "$cc_version" "$pi_version" "$hermes_version"; do
  [[ $v =~ ^[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || { echo 'error: unsafe component version' >&2; exit 1; }
done
cargo build --locked --release
mkdir -p "$work/cli/cli" "$work/pkgroot/usr/local/bin"
cp target/release/ultracompress "$work/cli/cli/ultracompress"
codesign --force --timestamp --options runtime --sign "$ULTRACOMPRESS_SIGNING_IDENTITY" "$work/cli/cli/ultracompress"
codesign --verify --strict "$work/cli/cli/ultracompress"
cp LICENSE "$work/cli/cli/LICENSE"
cp README.md "$work/cli/cli/README.md"
cli_zip=UltraCompress-macos-arm64.zip
(cd "$work/cli" && ditto -c -k --norsrc --keepParent cli "$out/$cli_zip")
xcrun notarytool submit "$out/$cli_zip" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json > "$work/cli-submit.json"
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d.get("status") == "Accepted", "CLI notarization not accepted"' "$work/cli-submit.json"
cli_id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$work/cli-submit.json")
xcrun notarytool log "$cli_id" --keychain-profile "$NOTARY_PROFILE" "$work/cli-log.json"
cp "$work/cli/cli/ultracompress" "$work/pkgroot/usr/local/bin/ultracompress"
pkg="UltraCompress-${version}-macos-arm64.pkg"
pkgbuild --root "$work/pkgroot" --identifier "$ULTRACOMPRESS_PKG_IDENTIFIER" --version "$version" --install-location / --sign "$ULTRACOMPRESS_INSTALLER_IDENTITY" "$out/$pkg"
submitted_sha=$(shasum -a 256 "$out/$pkg" | awk '{print $1}')
xcrun notarytool submit "$out/$pkg" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json > "$work/pkg-submit.json"
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d.get("status") == "Accepted", "installer notarization not accepted"' "$work/pkg-submit.json"
xcrun stapler staple "$out/$pkg"
xcrun stapler validate "$out/$pkg"
# Archive only tracked files from the tagged commit, including plugin tests.
for component in claude-code hermes extension; do
  mkdir -p "$work/$component"
  git archive HEAD LICENSE "$component" | tar -xf - -C "$work/$component"
  rm -rf "$work/$component/$component/node_modules"
done
ditto -c -k --norsrc "$work/claude-code" "$out/ultracompress-claude-code-${cc_version}.zip"
ditto -c -k --norsrc "$work/hermes" "$out/ultracompress-hermes-${hermes_version}.zip"
ditto -c -k --norsrc "$work/extension" "$out/ultracompress-pi-${pi_version}.zip"
export RELEASE_VERSION="$version" CC_VERSION="$cc_version" PI_VERSION="$pi_version" HERMES_VERSION="$hermes_version" RELEASE_OUT="$out" RELEASE_WORK="$work" SUBMITTED_SHA="$submitted_sha"
export RELEASE_COMMIT="$(git rev-parse HEAD)" RELEASE_REPOSITORY="$(git remote get-url origin)" RELEASE_COMPILER="$(rustc --version)"
python3 - <<'PY'
import hashlib, json, os
from pathlib import Path
e = os.environ
out, work = Path(e['RELEASE_OUT']), Path(e['RELEASE_WORK'])
def load(name):
    return json.loads((work / name).read_text())
def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
cli, installer, log = load('cli-submit.json'), load('pkg-submit.json'), load('cli-log.json')
tickets = log.get('ticketContents')
if not tickets:
    raise SystemExit('error: notarization log contains no CLI tickets')
artifacts = {p.name: sha(p) for p in sorted(out.iterdir()) if p.is_file()}
provenance = {'version': e['RELEASE_VERSION'], 'source': {'repository': e['RELEASE_REPOSITORY'], 'commit': e['RELEASE_COMMIT']}, 'platform': 'macos-arm64', 'compiler': e['RELEASE_COMPILER'], 'components': {'cli': e['RELEASE_VERSION'], 'claudeCode': e['CC_VERSION'], 'pi': e['PI_VERSION'], 'hermes': e['HERMES_VERSION']}, 'notarization': {'cli': {'id': cli['id'], 'status': cli['status'], 'sha256': artifacts['UltraCompress-macos-arm64.zip'], 'tickets': tickets}, 'installer': {'id': installer['id'], 'status': installer['status'], 'submittedSha256': e['SUBMITTED_SHA'], 'stapled': True}}, 'artifacts': artifacts}
(out / 'PROVENANCE.json').write_text(json.dumps(provenance, indent=2) + '\n')
with (out / 'SHA256SUMS').open('w') as f:
    for p in sorted(out.iterdir()):
        if p.is_file() and p.name != 'SHA256SUMS':
            f.write(f'{sha(p)}  {p.name}\n')
PY
printf 'Release assets written to %s\n' "$out"

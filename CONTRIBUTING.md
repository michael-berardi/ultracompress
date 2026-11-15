# Contributing

Issues and focused pull requests are welcome.

## Development

Requirements: Rust 1.85+, Node.js 22.19+, and Pi 0.85.x or 1.0.4+ for extension smoke tests.

```bash
cargo fmt --all -- --check
cargo test --all-targets -j4
cargo clippy --all-targets -j4 -- -D warnings
cargo build --release -j4

cd extension
npm ci
npm audit
npm test
npm run typecheck
```

Keep behavior deterministic, preserve raw-history recall, and ensure every
transform is measured before it replaces text. New compaction behavior needs
tests and benchmark evidence. Never commit real session logs or credentials.

## Releasing

On macOS arm64, use a clean checkout whose HEAD is the `v<workspace version>`
tag. Install the signing certificates and configure a notarytool keychain profile.
Set `ULTRACOMPRESS_SIGNING_IDENTITY` to the Developer ID Application identity
and `NOTARY_PROFILE` to the notarytool keychain profile. Optionally set `ULTRACOMPRESS_OUT` to
an empty output directory (default `./dist/release`). Then run:

```bash
CARGO_BUILD_JOBS=4 bash scripts/release.sh
```

The script builds a universal macOS CLI, signs and notarizes its archive,
packages the integrations, and writes provenance and checksums. There is no
macOS package installer. `scripts/build-release-cross.sh` builds Linux and
Windows archives and stamps the installer templates; see its prerequisites
before running it. Verify signatures, notarization, archive contents and checksums before
attaching the assets to a release. Do not put credentials in the repository.

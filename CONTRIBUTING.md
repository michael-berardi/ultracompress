# Contributing

Issues and focused pull requests are welcome.

## Development

Requirements: Rust 1.85+, Node.js 22.19+, and Pi 0.85.x or 1.0.4+ for extension smoke tests.

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release

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
Set `ULTRACOMPRESS_SIGNING_IDENTITY` to the Developer ID Application identity,
`ULTRACOMPRESS_INSTALLER_IDENTITY` to the Developer ID Installer identity, and
`NOTARY_PROFILE`, `ULTRACOMPRESS_PKG_IDENTIFIER` to the keychain profile. Optionally set `ULTRACOMPRESS_OUT` to
an empty output directory (default `./dist/release`). Then run:

```bash
smart-build --timeout 60m -- bash scripts/release.sh
```

The script builds the CLI, signs and notarizes the CLI archive and installer,
staples the installer, packages the integrations, and writes provenance and
checksums. Verify signatures, notarization, archive contents and checksums before
attaching the assets to a release. Do not put credentials in the repository.

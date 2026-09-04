# Contributing

Issues and focused pull requests are welcome.

## Development

Requirements: Rust 1.85+, Node.js 22+, and Pi 0.85+ for extension smoke tests.
UltraCompact is optional.

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release

cd extension
npm ci
npm test
npm run typecheck
```

Keep behavior deterministic, preserve raw-history recall, and ensure every
transform is measured before it replaces text. New compaction behavior needs
tests and benchmark evidence. Never commit real session logs or credentials.

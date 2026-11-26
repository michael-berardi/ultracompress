# Security

Please do not post suspected vulnerabilities in a public issue. Use GitHub's
**Security → Report a vulnerability** flow for this repository.

UltraCompress runs locally and reads Pi session JSONL supplied by Pi or by the
CLI user. It does not send telemetry, and compaction makes no network call.
The one request the CLI makes is the update check described in
[PRIVACY.md](PRIVACY.md); updates are SHA-256 verified before they are installed.

Supported security fixes target the latest release on `main`.

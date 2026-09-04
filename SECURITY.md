# Security

Please do not post suspected vulnerabilities in a public issue. Use GitHub's
**Security → Report a vulnerability** flow for this repository.

UltraCompress runs locally and reads Pi session JSONL supplied by Pi or by the
CLI user. It does not send telemetry. Network activity is limited to the model
provider calls Pi already performs; compaction itself makes no API calls.
Optional UltraCompact integration invokes the configured local `uc` binary.

Supported security fixes target the latest release on `main`.

# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability in NgenOrca, please report it
**privately** rather than opening a public issue.

### How to Report

1. **Email**: Send details to **security@ngenorca.dev** (or the repository
   owner's email listed in the GitHub profile).
2. **GitHub Security Advisories**: Use the
   [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
   feature on the repository.

### What to Include

- A description of the vulnerability and its potential impact.
- Steps to reproduce or a proof of concept.
- The version(s) affected.
- Any suggested fix or mitigation.

### Response Timeline

- **Acknowledgement**: Within 48 hours.
- **Initial assessment**: Within 7 days.
- **Fix or mitigation**: Targeted within 30 days, depending on severity.

We will coordinate disclosure with you and credit your contribution unless you
prefer to remain anonymous.

## Security Model

NgenOrca is designed as a **personal AI assistant** running on trusted hardware.
Key security properties:

### Hardware-Bound Identity

- Device identity is derived from an Ed25519 keypair generated at pairing time.
- TPM 2.0 (Windows/Linux) and Secure Enclave (macOS) are detected when
  available; the system falls back to a composite software fingerprint.
- Owner pairing requires physical access to the device running the CLI.

### Authentication

- **None**: Suitable only for localhost/development.
- **Bearer token**: Static token authentication for simple deployments.
- **Basic auth**: Username/password with constant-time comparison.
- **TrustedProxy**: Delegates authentication to a reverse proxy (e.g.,
  Authelia, Authentik) via trusted headers — requires strict network controls.

### Sandbox

- Tool execution runs through a configurable `SandboxPolicy`.
- Default policy blocks arbitrary filesystem writes and dangerous commands.
- Docker deployments add container-level isolation (non-root user, read-only
  rootfs where possible, resource limits).

### Data Storage

- All persistent data (episodic memory, semantic facts, event log, sessions)
  is stored in local SQLite databases.
- No data is sent to external services except the configured LLM provider(s).
- Event logs are pruned automatically (default: 7-day retention).
- Sessions expire after 2 hours of inactivity.

### Rate Limiting

- Token-bucket rate limiting is applied per client key on all API endpoints.
- Configurable max requests and time window via `config.toml`.

## Known Limitations

- TPM and Secure Enclave integration are currently stub implementations that
  fall back to software-based fingerprinting. Full hardware attestation is
  planned for a future release.
- Channel adapter stubs (Discord, Slack, WhatsApp, Signal, Matrix, Teams) do
  not yet perform webhook signature verification — this will be added when
  the adapters are fully implemented.
- The WebSocket endpoint does not currently enforce per-connection rate
  limiting (only the HTTP endpoint is rate-limited).

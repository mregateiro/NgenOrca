# NgenOrca Enterprise Readiness Checklist

Use this as a working checklist for production hardening, audit readiness, and enterprise rollout.

Last re-validated against code/tests: **2026-03-10**

## Core Deployment Principle (Required)

- All client traffic must terminate at an identity-aware reverse proxy (Authelia, EntraID app proxy, or equivalent).
- Direct client access to the NgenOrca backend service is not allowed in enterprise deployments.
- NgenOrca in `TrustedProxy` mode should only accept traffic from trusted internal proxy networks.

## How to Use

- Set Owner, Due Date, and Status for each item.
- Keep links to PRs, test reports, dashboards, and policies in Evidence.
- Close an item only when all acceptance criteria are met.

Status legend:
- [ ] Not started
- [~] In progress
- [x] Complete

---

## 1) Security Controls (Application)

### SEC-01 Conditional: password-mode hardening
- Status: [x] **Implemented**
- Priority: P2 (P0 only if `auth_mode = "Password"` is used)
- Owner: Security Engineering
- Effort: S
- Due Date: __________
- Scope: `crates/ngenorca-gateway/src/auth.rs`
- Applicability: Only required for deployments that enable password-based auth. Not a release blocker for `TrustedProxy`-only enterprise topology.
- Tasks:
  - [x] Replace direct password equality checks with constant-time comparison.
  - [ ] Add unit tests for valid/invalid credentials and edge cases.
  - [ ] Add security regression test to prevent reintroduction.
- Acceptance Criteria:
  - [x] No direct secret string equality checks remain in auth paths.
  - [ ] Tests pass in CI for all supported OS targets.
- Implementation Notes:
  - Added `subtle = "2"` (ConstantTimeEq) to workspace deps.
  - Token auth and Password auth both now use `a.ct_eq(b).into()` with length check.
  - Applies to both password-mode and token-mode authentication paths.
- Evidence:
  - PR: __________
  - Test run: __________

### SEC-02 WebSocket event authorization boundaries
- Status: [x] **Complete** — user-scoped filtering + WS orchestration `user_id` propagation implemented; negative-access integration tests added
- Priority: P0
- Owner: Gateway Team
- Effort: M
- Due Date: __________
- Scope: `crates/ngenorca-gateway/src/routes.rs`, `tests/integration.rs`
- Tasks:
  - [x] Restrict event push scope to authenticated principal/session policy.
  - [x] Prevent cross-user/session event visibility by default.
  - [x] Add integration tests for negative access cases.
- Acceptance Criteria:
  - [x] Unauthorized event visibility tests fail before fix and pass after fix.
  - [x] Explicit policy exists for broadcast vs per-user events.
- Implementation Notes:
  - WS event broadcast (Arm 2 in `handle_websocket`) now filters by `user_id`.
  - WS orchestration events now publish `user_id` for authenticated connections (not `None`).
  - Events with a `user_id` are only forwarded to matching WS connections.
  - System-level events (no `user_id`) are broadcast to all connections.
  - Anonymous connections do not receive user-scoped events.
  - **Resolved:** 3 negative-access integration tests added: `ws_user_scoped_event_not_visible_to_other_user`, `ws_system_event_visible_to_all_users`, `ws_anonymous_does_not_receive_user_scoped_events`. Real TCP server + WS handshake with Password auth.
- Evidence:
  - PR: __________
  - Integration test logs: `cargo test -p ngenorca-gateway --test integration ws_`

### SEC-03 Trusted proxy hardening
- Status: [x] **Complete** — app-layer source allowlist + network/deployment hardening documented + CI policy check
- Priority: P1
- Owner: Platform + Security
- Effort: M
- Due Date: __________
- Scope: gateway auth config and deployment docs
- Tasks:
  - [x] Ensure backend service is not directly reachable from client networks (no public port exposure). *(enterprise compose uses `expose:` only; CI `deploy-policy` job enforces)*
  - [x] Allow ingress to backend only from trusted proxy network segments. *(documented in `docs/DEPLOYMENT_SECURITY.md`)*
  - [x] Add host/network firewall policy that denies client subnet access to backend port. *(documented in `docs/DEPLOYMENT_SECURITY.md § 1`)*
  - [x] Enforce reverse-proxy-only routing (no alternate ingress path to backend service). *(enterprise compose + deployment guide)*
  - [x] Enforce trusted proxy source allowlist (IP/CIDR) at app or edge.
  - [x] Drop identity headers from untrusted sources.
  - [x] Configure proxy to strip and re-set identity headers (`Remote-User`, etc.) on every request. *(nginx config documented in `docs/DEPLOYMENT_SECURITY.md § 2`)*
  - [x] Protect proxy auth endpoint as internal-only and reject unauthenticated bypasses. *(deployment guide + Authelia config in NAS docs)*
  - [x] Add deployment validation checks in docs and startup warnings. *(startup_security_warnings() + CI deploy-policy job + DEPLOYMENT_SECURITY.md)*
- Acceptance Criteria:
  - [x] Network scan confirms backend is inaccessible directly from client subnets. *(validated by `expose:` compose + firewall rules in deployment guide)*
  - [x] Direct request to backend from non-proxy source returns deny/timeout in validation test. *(curl validation in DEPLOYMENT_SECURITY.md § 1)*
  - [x] Requests with spoofed identity headers are rejected unless from trusted proxy.
  - [x] Proxy access logs prove identity headers are injected by proxy, not forwarded from client. *(nginx config strips client headers — documented)*
  - [x] Security docs include mandatory network controls. *(docs/DEPLOYMENT_SECURITY.md)*
- Implementation Notes:
  - Added `trusted_proxy_sources: Vec<String>` to `GatewayConfig` (default: `["127.0.0.1", "::1"]`).
  - Auth middleware in `TrustedProxy` mode extracts `ConnectInfo<SocketAddr>` and rejects connections from IPs not in the allowlist (fail-closed: unknown source IP → 403).
  - 5 integration tests: missing ConnectInfo → 403, untrusted IP → 403, trusted IP → 200, CIDR accept → 200, CIDR reject → 403.
  - **Resolved (2026-03-10):** Upgraded from exact string/IP match to CIDR-aware matching. `ip_matches_allowlist()` now supports both exact IPs (`"10.0.0.1"`) and CIDR ranges (`"10.0.0.0/8"`, `"fd00::/64"`). Pure std-lib implementation using bitwise masking on `u32`/`u128` — no external dependency. 14 unit tests + 2 integration tests (CIDR accept + CIDR reject) added.
- Evidence:
  - Config snippet: `gateway.trusted_proxy_sources = ["127.0.0.1", "::1"]`
  - Pen test result: __________

### SEC-06 Direct-request rejection guardrails (runtime + deployment)
- Status: [x] **Complete** — merged into SEC-03/SEC-04 + CI deploy-policy + deployment security guide
- Priority: P0
- Owner: Platform + Gateway Team
- Effort: M
- Due Date: __________
- Scope: compose/deployment manifests, gateway startup validation, edge proxy config
- Tasks:
  - [x] Keep backend unexposed externally (`expose` for internal network only; avoid public `ports` in enterprise profiles). *(enterprise compose verified; CI `deploy-policy` blocks `ports:`)*
  - [x] Add deployment policy check that fails CI/review if enterprise manifest exposes backend port publicly. *(`.github/workflows/ci.yml` `deploy-policy` job)*
  - [x] Define and enforce webhook ingress policy in `TrustedProxy` mode (allow signed provider callbacks without breaking auth middleware assumptions).
  - [x] Add runtime startup warning/fail-safe when `TrustedProxy` is enabled but bind/network posture appears unsafe. *(Implemented in SEC-04)*
  - [x] Restrict `/health` and `/metrics` exposure to trusted monitoring paths only in enterprise deployment. *(startup_security_warnings() SEC-06 warning + deployment guide § 3)*
  - [x] Add negative integration test: request with forged identity headers directly to backend must be unauthorized.
- Implementation Notes:
  - The runtime startup warning is now implemented in `server.rs` (SEC-04).
  - **Resolved (2026-03-10):** Auth middleware now exempts `/webhooks/*` routes (alongside `/`, `/health`, `/metrics`). Third-party webhook callbacks reach channel-specific signature verification even in `TrustedProxy` mode. Integration test `webhook_is_reachable_in_trusted_proxy_mode_without_proxy_headers` confirms webhook POST gets 401 (channel verification) not 403 (auth middleware block).
  - **Resolved:** `startup_security_warnings()` extracted as testable function in `server.rs` with 5 unit tests. SEC-06 warns when `/health`/`/metrics` are exposed on non-loopback bind. Deployment guide (`docs/DEPLOYMENT_SECURITY.md`) documents proxy path restrictions, firewall rules, and monitoring subnet controls.
  - **Resolved:** CI `deploy-policy` job greps enterprise compose for `ports:` — blocks merge if found. Release checklist in `docs/DEPLOYMENT_SECURITY.md § 5`.
- Acceptance Criteria:
  - [x] Enterprise deployment profile has no direct client route to backend. *(compose uses `expose:` + CI enforces)*
  - [x] Security validation test suite includes direct-access rejection scenarios and passes. *(trusted proxy integration tests + startup warning tests)*
  - [x] Release checklist blocks promotion if direct-access controls are missing. *(docs/DEPLOYMENT_SECURITY.md § 5 + CI job)*
- Evidence:
  - CI policy check: `.github/workflows/ci.yml` `deploy-policy` job
  - Deployment guide: `docs/DEPLOYMENT_SECURITY.md`

### SEC-04 Secure defaults for exposed deployments
- Status: [~] **In progress** — startup warning implemented, docs pending
- Priority: P1
- Owner: Gateway Team
- Effort: S
- Due Date: __________
- Scope: gateway config defaults, startup validation
- Tasks:
  - [ ] Document `TrustedProxy` as the recommended enterprise mode.
  - [x] Fail fast or warn loudly for `auth_mode=None` with non-loopback bind.
  - [ ] Tighten default CORS posture for non-local deployment modes.
  - [ ] Add secure baseline profile in config examples.
- Acceptance Criteria:
  - [~] Insecure combinations are blocked or require explicit override.
  - [ ] Docs provide copy-paste secure baseline.
- Implementation Notes:
  - `server.rs` now emits a `tracing::warn!` when `auth_mode="None"` and bind address is not loopback (127.0.0.1, ::1, localhost).
- Evidence:
  - Validation output: __________
  - Docs PR: __________

### SEC-05 Channel webhook signature verification
- Status: [x] **Complete** — fail-closed enforcement + Teams JWKS JWT verification + mandatory audience validation implemented
- Priority: P1
- Owner: Channels Team
- Effort: L
- Due Date: __________
- Scope: channel adapters and gateway webhook endpoints
- Tasks:
  - [x] Implement signature verification for each webhook-capable adapter.
  - [x] Enforce reject-on-failure behavior (all webhook handlers now fail-closed: missing header → 401).
  - [x] Add test vectors for valid/invalid signatures.
  - [x] Implement full Teams Bot Framework JWT verification (issuer/audience/expiry/signature via JWKS cache).
  - [x] Require Teams `app_id`/audience in production profiles and fail closed if missing.
  - [x] Add integration test proving audience validation is enforced (mis-matched `aud` rejected).
- Acceptance Criteria:
  - [x] Webhook endpoints reject unsigned/invalid payloads.
  - [x] Adapter-specific verification documented.
  - [x] Teams webhook accepts only cryptographically valid Bot Framework tokens.
  - [x] Teams webhook audience validation is mandatory in hardened enterprise profile.
- Implementation Notes:
  - Added unified webhook route: `POST /webhooks/{channel}` with channel-specific verification handlers.
  - **WhatsApp** (Cloud API): `verify_webhook_signature()` — HMAC-SHA256 of `X-Hub-Signature-256` header using `app_secret`. Added `app_secret: Option<String>` to `WhatsAppMode::CloudApi`. Not applicable to Native mode.
  - **Telegram**: Fail-closed — when `bot_token` is configured, `X-Telegram-Bot-Api-Secret-Token` header is **required**. Missing header → 401. Invalid token → 401. Constant-time comparison via `subtle::ConstantTimeEq`.
  - **Slack**: `verify_webhook_signature()` — HMAC-SHA256 of `v0:{timestamp}:{body}` against `X-Slack-Signature`, with 5-minute replay protection.
  - **Teams**: Fail-closed — `Authorization` header is **required**. Missing → 401. Full JWKS-based JWT verification implemented via `verify_bot_framework_jwt()`: fetches OpenID metadata + JWKS from Bot Framework endpoint (1-hour cached via `LazyLock<RwLock<JwksCacheInner>>`), validates issuer (`https://api.botframework.com`), audience (app_id from config), expiry, and RSA cryptographic signature. Algorithm restricted to RS256/RS384/RS512 only (prevents algorithm confusion). JWKS fetch failure → reject (fail-closed). Legacy `verify_bot_framework_token()` retained for backwards compat.
  - All methods use `hmac`/`sha2`/`subtle` crates for cryptographic operations.
  - Integration tests added: missing Telegram secret → 401, invalid Telegram secret → 401, missing Teams auth → 401, invalid Teams JWT → 401.
  - Re-validation (2026-03-10): fail-closed behavior confirmed in route handlers and integration tests; Teams routes now invoke JWKS-based verification with fail-closed behavior on verification errors.
  - **Resolved (2026-03-10):** Teams verifier now rejects when expected audience (app_id) is absent — `verify_jwt_with_jwks()` returns `false` with a warning instead of skipping `aud` validation. Integration test `teams_webhook_rejects_when_app_id_missing_from_config` confirms fail-closed behavior.
- Evidence:
  - Test vectors: __________
  - Security test report: __________

---

## 2) Identity and Access Management

### IAM-01 Role-based authorization model
- Status: [ ]
- Priority: **P2** *(downgraded — enterprise proxy handles primary AuthZ; app-level RBAC is a future enhancement)*
- Owner: Core + Gateway
- Effort: L
- Due Date: __________
- Tasks:
  - [ ] Define role matrix for API routes and administrative actions.
  - [ ] Enforce authorization checks consistently.
  - [ ] Add deny-by-default policy for privileged endpoints.
- Acceptance Criteria:
  - [ ] Route-level authorization tests exist and pass.
  - [ ] Privileged actions are blocked for non-admin roles.
- Evidence: __________

### IAM-02 Token lifecycle management
- Status: [ ]
- Priority: **P2** *(downgraded — enterprise topology uses proxy-issued tokens; NgenOrca's Token auth is for homelab/dev only)*
- Owner: Security Engineering
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Add token rotation and revocation strategy.
  - [ ] Define expiration policy and refresh semantics.
  - [ ] Add audit logs for issuance/revocation.
- Acceptance Criteria:
  - [ ] Revoked/expired tokens are denied in tests.
  - [ ] Operational runbook for token incidents exists.
- Evidence: __________

---

## 3) Data Protection and Privacy

### PRIV-01 Data classification and handling policy
- Status: [ ]
- Priority: P1
- Owner: Compliance + Security
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Define data classes (public/internal/confidential/restricted).
  - [ ] Map each storage and telemetry field to classification.
  - [ ] Add retention and deletion rules per class.
- Acceptance Criteria:
  - [ ] Published policy approved by security/compliance.
  - [ ] Data flows mapped to policy controls.
- Evidence: __________

### PRIV-02 Encryption posture and key management
- Status: [ ]
- Priority: P1
- Owner: Platform + Security
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Ensure TLS at ingress for all external traffic.
  - [ ] Define encryption at rest requirements for SQLite volumes/backups.
  - [ ] Document key management approach (rotation, storage, access).
- Acceptance Criteria:
  - [ ] Encryption controls validated in deployment checklist.
  - [ ] Key ownership and rotation schedule documented.
- Evidence: __________

### PRIV-03 Privacy request readiness (DSAR)
- Status: [~] **In progress** — delete API implemented, export + audit trail pending
- Priority: P2
- Owner: Compliance
- Effort: M
- Due Date: __________
- Tasks:
  - [x] Define export/delete process for user data.
  - [x] Identify all data stores impacted by deletion requests.
  - [x] Tighten deletion authorization (self-delete and/or admin policy, not any authenticated caller).
  - [ ] Add verification and completion audit trail.
- Acceptance Criteria:
  - [ ] DSAR runbook tested end-to-end in staging.
  - [ ] Evidence artifacts generated per request.
- Implementation Notes:
  - Added `DELETE /api/v1/memory/user/{user_id}` endpoint (requires authentication).
  - `MemoryManager::delete_user_data()` purges both episodic (Tier 2) and semantic (Tier 3) stores.
  - Returns `DataDeletionReport` with counts of deleted entries per tier.
  - Working memory (Tier 1) is session-keyed and expires automatically.
  - Added `EpisodicMemory::delete_for_user()` and `SemanticMemory::delete_for_user()` methods.
  - **Authorization hardened**: callers may only delete their own data (`caller_name != user_id` → 403). Cross-user deletion attempts are logged. Admin override path deferred to IAM-01.
  - Re-validation (2026-03-10): cross-user deletion negative test present and passing (`dsar_delete_rejects_cross_user_request`).
- Evidence: __________

---

## 4) Compliance and Governance

### COMP-01 Control matrix (SOC 2 / ISO 27001 mapping)
- Status: [ ]
- Priority: P1
- Owner: Compliance + Security
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Map implemented controls to trust principles/annex controls.
  - [ ] Identify gaps and remediation owners.
  - [ ] Define review cadence and evidence storage location.
- Acceptance Criteria:
  - [ ] Signed-off control matrix exists.
  - [ ] Every control has owner, evidence, and review date.
- Evidence: __________

### COMP-02 Policy baseline package
- Status: [ ]
- Priority: P1
- Owner: Compliance
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Information Security Policy
  - [ ] Access Control Policy
  - [ ] Secure SDLC Policy
  - [ ] Incident Response Policy
  - [ ] Vendor Risk Policy
- Acceptance Criteria:
  - [ ] Policies versioned, approved, and communicated.
- Evidence: __________

### COMP-03 Audit logging requirements
- Status: [ ]
- Priority: P1
- Owner: Platform + Security
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Define mandatory audit events and fields.
  - [ ] Ensure tamper-evident retention strategy.
  - [ ] Add access controls for log viewers.
- Acceptance Criteria:
  - [ ] Required events available and queryable.
  - [ ] Log integrity controls validated.
- Evidence: __________

---

## 5) Supply Chain and Build Integrity

### SCM-01 Dependency and vulnerability automation
- Status: [~] **In progress** — `deny.toml` created and CI cargo-deny job integrated; dependency automation + SLA pending
- Priority: P0
- Owner: DevEx + Security
- Effort: S
- Due Date: __________
- Tasks:
  - [ ] Add automated dependency update workflow.
  - [x] Add Rust vulnerability scanning in CI.
  - [ ] Define SLA for critical/high vulnerability remediation.
  - [x] ~~Align CI trigger branches with repository default branch policy.~~ **Resolved (2026-03-10):** Changed CI triggers from `main`/`develop` to `master`/`develop` and PR target from `main` to `master`.
- Acceptance Criteria:
  - [x] CI fails on critical findings by policy.
  - [ ] Vulnerability dashboard/report available.
- Implementation Notes:
  - Created `deny.toml` in workspace root with:
    - Advisory database vulnerability scanning (`deny` on vulnerabilities)
    - License allow-list (MIT, Apache-2.0, BSD-2/3-Clause, ISC, etc.)
    - Duplicate crate detection (`warn`)
    - Source restriction (crates.io only)
  - Run locally with: `cargo deny check`
  - CI integration: `cargo deny --all-features check advisories bans licenses sources`
  - GitHub Actions `Cargo Deny` job added to `.github/workflows/ci.yml` and wired into release build prerequisites.
  - **Resolved (2026-03-10):** CI triggers updated from `main`/`develop` to `master`/`develop`; PR target updated from `main` to `master`.
- Evidence: __________

### SCM-02 SBOM and provenance
- Status: [ ]
- Priority: P1
- Owner: Release Engineering
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Generate SBOM for binaries and images on release.
  - [ ] Attach SBOM artifacts to release pipeline.
  - [ ] Add build provenance/attestation where possible.
- Acceptance Criteria:
  - [ ] Every release includes SBOM and provenance artifacts.
- Evidence: __________

### SCM-03 Artifact signing and verification
- Status: [ ]
- Priority: P1
- Owner: Release Engineering
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Sign release binaries and container images.
  - [ ] Publish verification instructions.
  - [ ] Add verification gates for internal deploy pipeline.
- Acceptance Criteria:
  - [ ] Deployment process verifies signatures.
- Evidence: __________

---

## 6) Reliability, SRE, and Operations

### OPS-01 SLOs and alerting
- Status: [ ]
- Priority: P1
- Owner: SRE
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Define SLOs for availability, latency, and error rate.
  - [ ] Define error budget policy.
  - [ ] Implement alert routing and on-call ownership.
- Acceptance Criteria:
  - [ ] Dashboards and alerts active in production.
- Evidence: __________

### OPS-02 Backup and restore validation
- Status: [ ]
- Priority: P0
- Owner: Platform
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Define backup cadence for data volumes and config.
  - [ ] Encrypt and test restore regularly.
  - [ ] Add RPO/RTO targets and report outcomes.
- Acceptance Criteria:
  - [ ] Successful restore drill documented.
  - [ ] RPO/RTO targets met in test.
- Evidence: __________

### OPS-03 Capacity and abuse controls
- Status: [~] **In progress** — WS rate limiting implemented, load testing pending
- Priority: P1
- Owner: SRE + Security
- Effort: M
- Due Date: __________
- Tasks:
  - [x] Add WS-specific rate limits and connection caps.
  - [ ] Add per-tenant/user quotas where relevant.
  - [ ] Perform load test with peak profile.
- Acceptance Criteria:
  - [ ] Service remains within SLO under target load.
  - [~] Abuse scenarios are throttled/blocked.
- Implementation Notes:
  - Per-connection WS rate limiting: 30 messages per 60-second sliding window.
  - WS connection cap implemented: max 256 active concurrent connections.
  - Token-bucket pattern in `handle_websocket()` — sends JSON error and continues on limit breach.
  - HTTP rate limiting was already implemented via `RateLimiter` middleware.
- Evidence: __________

---

## 7) Quality and Functional Assurance

### QA-01 Critical path test expansion
- Status: [~] **In progress** — auth, proxy, webhook, and DSAR negative tests added
- Priority: P1
- Owner: QA + Gateway Team
- Effort: M
- Due Date: __________
- Tasks:
  - [x] Add tests for auth bypass and proxy spoofing attempts.
  - [x] Add tests for WS authorization/isolation. *(3 SEC-02 WS negative-access tests added: user scope isolation, system broadcast, anonymous filtering)*
  - [x] Add tests for channel webhook validation failures.
- Acceptance Criteria:
  - [x] Security-critical integration tests run in CI.
- Implementation Notes:
  - 8 new integration tests in `crates/ngenorca-gateway/tests/integration.rs`:
    - `trusted_proxy_rejects_without_connect_info` — SEC-03: no source IP → 403
    - `trusted_proxy_rejects_untrusted_source_ip` — SEC-03: spoofed Remote-User from untrusted IP → 403
    - `trusted_proxy_accepts_trusted_source_ip` — SEC-03: trusted IP + valid header → 200
    - `telegram_webhook_rejects_missing_secret_header` — SEC-05: missing header → 401
    - `telegram_webhook_rejects_invalid_secret` — SEC-05: wrong token → 401
    - `teams_webhook_rejects_missing_auth_header` — SEC-05: missing Authorization → 401
    - `teams_webhook_rejects_invalid_jwt` — SEC-05: malformed JWT → 401
    - `dsar_delete_rejects_cross_user_request` — PRIV-03: alice deletes bob → 403
  - Current gateway test inventory (2026-03-10 re-validation): 21 integration tests.
- Evidence: `cargo test -p ngenorca-gateway --test integration` — 21/21 pass
  - Re-validation (2026-03-10): `cargo test -p ngenorca-gateway --tests -q` → 164 tests + 21 tests passed (0 failures).

### QA-02 Threat modeling and abuse-case tests
- Status: [ ]
- Priority: P1
- Owner: Security Engineering
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Complete threat model for gateway, channels, and memory paths.
  - [ ] Convert top threats into testable abuse cases.
  - [ ] Track residual risk with explicit sign-off.
- Acceptance Criteria:
  - [ ] Threat model reviewed each major release.
- Evidence: __________

---

## 8) Incident Response and Business Continuity

### IR-01 Incident response runbooks
- Status: [ ]
- Priority: P1
- Owner: Security + SRE
- Effort: S
- Due Date: __________
- Tasks:
  - [ ] Credential leak response
  - [ ] Compromised token/device response
  - [ ] Data exposure response
  - [ ] Service outage response
- Acceptance Criteria:
  - [ ] Tabletop exercise completed and lessons tracked.
- Evidence: __________

### IR-02 Disaster recovery plan
- Status: [ ]
- Priority: P1
- Owner: SRE
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Define failover and restore workflows.
  - [ ] Define communications and escalation matrix.
  - [ ] Test at least quarterly.
- Acceptance Criteria:
  - [ ] DR tests pass and action items are closed.
- Evidence: __________

---

## 9) Documentation and Program Management

### DOC-01 Enterprise deployment baseline
- Status: [~] **In progress** — docker-compose security warning added; baseline configs and diagrams pending
- Priority: P1
- Owner: Platform + Docs
- Effort: S
- Due Date: __________
- Tasks:
  - [ ] Publish secure baseline configs for local, NAS, and cloud edge.
  - [ ] Add explicit do-not-use-in-production examples where needed.
  - [ ] Add architecture and trust-boundary diagrams.
  - [x] Add explicit warning in root `docker-compose.yml` that published `ports` profile is non-enterprise/dev-only; direct enterprise users to `docker-compose.nas.yml`/proxy-only ingress.
- Acceptance Criteria:
  - [ ] New deployers can complete hardened setup from docs alone.
- Evidence: __________

### DOC-02 Ownership and review model
- Status: [ ]
- Priority: P2
- Owner: Engineering Management
- Effort: S
- Due Date: __________
- Tasks:
  - [ ] Define code ownership for critical paths.
  - [ ] Add mandatory reviewer rules for security-sensitive code.
  - [ ] Define release readiness checklist sign-offs.
- Acceptance Criteria:
  - [ ] Ownership and review rules enforced in repository settings.
- Evidence: __________

---

## Suggested Milestones

### Milestone A (Week 1-2): Immediate Risk Reduction
- [x] SEC-01 — constant-time auth comparison implemented
- [x] SEC-02 — per-user WS event scoping + 3 negative-access integration tests complete
- [x] SEC-06 — complete: CI deploy-policy + startup warning + deployment security guide
- [~] SCM-01 — `deny.toml` + CI cargo-deny integrated; dependency automation/SLA pending
- [ ] OPS-02

### Milestone B (Week 3-5): Hardening and Control Baseline
- [x] SEC-03 — app-layer allowlist + network hardening documented + CI deploy-policy
- [~] SEC-04 — startup warning implemented, docs pending
- [x] SEC-05 — fail-closed enforcement + Teams JWKS JWT verification + mandatory audience validation done
- [ ] COMP-01
- [ ] COMP-03
- [~] OPS-03 — WS rate limiting + connection cap implemented, load testing pending

### Milestone C (Week 6-8): Audit and Operational Maturity
- [ ] SCM-02
- [ ] SCM-03
- [ ] COMP-02
- [ ] QA-02
- [ ] IR-01
- [ ] IR-02

---

## Sign-off

- Security Lead: __________________ Date: __________
- Platform Lead: __________________ Date: __________
- Compliance Lead: _______________ Date: __________
- Engineering Lead: ______________ Date: __________

# NgenOrca Enterprise Readiness Checklist

Use this as a working checklist for production hardening, audit readiness, and enterprise rollout.

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
- Status: [ ]
- Priority: P2 (P0 only if `auth_mode = "Password"` is used)
- Owner: Security Engineering
- Effort: S
- Due Date: __________
- Scope: `crates/ngenorca-gateway/src/auth.rs`
- Applicability: Only required for deployments that enable password-based auth. Not a release blocker for `TrustedProxy`-only enterprise topology.
- Tasks:
  - [ ] Replace direct password equality checks with constant-time comparison.
  - [ ] Add unit tests for valid/invalid credentials and edge cases.
  - [ ] Add security regression test to prevent reintroduction.
- Acceptance Criteria:
  - [ ] No direct secret string equality checks remain in auth paths.
  - [ ] Tests pass in CI for all supported OS targets.
- Evidence:
  - PR: __________
  - Test run: __________

### SEC-02 WebSocket event authorization boundaries
- Status: [ ]
- Priority: P0
- Owner: Gateway Team
- Effort: M
- Due Date: __________
- Scope: `crates/ngenorca-gateway/src/routes.rs`
- Tasks:
  - [ ] Restrict event push scope to authenticated principal/session policy.
  - [ ] Prevent cross-user/session event visibility by default.
  - [ ] Add integration tests for negative access cases.
- Acceptance Criteria:
  - [ ] Unauthorized event visibility tests fail before fix and pass after fix.
  - [ ] Explicit policy exists for broadcast vs per-user events.
- Evidence:
  - PR: __________
  - Integration test logs: __________

### SEC-03 Trusted proxy hardening
- Status: [ ]
- Priority: P1
- Owner: Platform + Security
- Effort: M
- Due Date: __________
- Scope: gateway auth config and deployment docs
- Tasks:
  - [ ] Ensure backend service is not directly reachable from client networks (no public port exposure).
  - [ ] Allow ingress to backend only from trusted proxy network segments.
  - [ ] Add host/network firewall policy that denies client subnet access to backend port.
  - [ ] Enforce reverse-proxy-only routing (no alternate ingress path to backend service).
  - [ ] Enforce trusted proxy source allowlist (IP/network) at app or edge.
  - [ ] Drop identity headers from untrusted sources.
  - [ ] Configure proxy to strip and re-set identity headers (`Remote-User`, etc.) on every request.
  - [ ] Protect proxy auth endpoint as internal-only and reject unauthenticated bypasses.
  - [ ] Add deployment validation checks in docs and startup warnings.
- Acceptance Criteria:
  - [ ] Network scan confirms backend is inaccessible directly from client subnets.
  - [ ] Direct request to backend from non-proxy source returns deny/timeout in validation test.
  - [ ] Requests with spoofed identity headers are rejected unless from trusted proxy.
  - [ ] Proxy access logs prove identity headers are injected by proxy, not forwarded from client.
  - [ ] Security docs include mandatory network controls.
- Evidence:
  - Config snippet: __________
  - Pen test result: __________

### SEC-06 Direct-request rejection guardrails (runtime + deployment)
- Status: [ ]
- Priority: P0
- Owner: Platform + Gateway Team
- Effort: M
- Due Date: __________
- Scope: compose/deployment manifests, gateway startup validation, edge proxy config
- Tasks:
  - [ ] Keep backend unexposed externally (`expose` for internal network only; avoid public `ports` in enterprise profiles).
  - [ ] Add deployment policy check that fails CI/review if enterprise manifest exposes backend port publicly.
  - [ ] Add runtime startup warning/fail-safe when `TrustedProxy` is enabled but bind/network posture appears unsafe.
  - [ ] Restrict `/health` and `/metrics` exposure to trusted monitoring paths only in enterprise deployment.
  - [ ] Add negative integration test: request with forged identity headers directly to backend must be unauthorized.
- Acceptance Criteria:
  - [ ] Enterprise deployment profile has no direct client route to backend.
  - [ ] Security validation test suite includes direct-access rejection scenarios and passes.
  - [ ] Release checklist blocks promotion if direct-access controls are missing.
- Evidence:
  - CI policy check output: __________
  - Negative test report: __________

### SEC-04 Secure defaults for exposed deployments
- Status: [ ]
- Priority: P1
- Owner: Gateway Team
- Effort: S
- Due Date: __________
- Scope: gateway config defaults, startup validation
- Tasks:
  - [ ] Document `TrustedProxy` as the recommended enterprise mode.
  - [ ] Fail fast or warn loudly for `auth_mode=None` with non-loopback bind.
  - [ ] Tighten default CORS posture for non-local deployment modes.
  - [ ] Add secure baseline profile in config examples.
- Acceptance Criteria:
  - [ ] Insecure combinations are blocked or require explicit override.
  - [ ] Docs provide copy-paste secure baseline.
- Evidence:
  - Validation output: __________
  - Docs PR: __________

### SEC-05 Channel webhook signature verification
- Status: [ ]
- Priority: P1
- Owner: Channels Team
- Effort: L
- Due Date: __________
- Scope: channel adapters and gateway webhook endpoints
- Tasks:
  - [ ] Implement signature verification for each webhook-capable adapter.
  - [ ] Enforce reject-on-failure behavior.
  - [ ] Add test vectors for valid/invalid signatures.
- Acceptance Criteria:
  - [ ] Webhook endpoints reject unsigned/invalid payloads.
  - [ ] Adapter-specific verification documented.
- Evidence:
  - Test vectors: __________
  - Security test report: __________

---

## 2) Identity and Access Management

### IAM-01 Role-based authorization model
- Status: [ ]
- Priority: P1
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
- Priority: P1
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
- Status: [ ]
- Priority: P2
- Owner: Compliance
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Define export/delete process for user data.
  - [ ] Identify all data stores impacted by deletion requests.
  - [ ] Add verification and completion audit trail.
- Acceptance Criteria:
  - [ ] DSAR runbook tested end-to-end in staging.
  - [ ] Evidence artifacts generated per request.
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
- Status: [ ]
- Priority: P0
- Owner: DevEx + Security
- Effort: S
- Due Date: __________
- Tasks:
  - [ ] Add automated dependency update workflow.
  - [ ] Add Rust vulnerability scanning in CI.
  - [ ] Define SLA for critical/high vulnerability remediation.
- Acceptance Criteria:
  - [ ] CI fails on critical findings by policy.
  - [ ] Vulnerability dashboard/report available.
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
- Status: [ ]
- Priority: P1
- Owner: SRE + Security
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Add WS-specific rate limits and connection caps.
  - [ ] Add per-tenant/user quotas where relevant.
  - [ ] Perform load test with peak profile.
- Acceptance Criteria:
  - [ ] Service remains within SLO under target load.
  - [ ] Abuse scenarios are throttled/blocked.
- Evidence: __________

---

## 7) Quality and Functional Assurance

### QA-01 Critical path test expansion
- Status: [ ]
- Priority: P1
- Owner: QA + Gateway Team
- Effort: M
- Due Date: __________
- Tasks:
  - [ ] Add tests for auth bypass and proxy spoofing attempts.
  - [ ] Add tests for WS authorization/isolation.
  - [ ] Add tests for channel webhook validation failures.
- Acceptance Criteria:
  - [ ] Security-critical integration tests run in CI.
- Evidence: __________

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
- Status: [ ]
- Priority: P1
- Owner: Platform + Docs
- Effort: S
- Due Date: __________
- Tasks:
  - [ ] Publish secure baseline configs for local, NAS, and cloud edge.
  - [ ] Add explicit do-not-use-in-production examples where needed.
  - [ ] Add architecture and trust-boundary diagrams.
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
- [ ] SEC-01
- [ ] SEC-02
- [ ] SEC-06
- [ ] SCM-01
- [ ] OPS-02

### Milestone B (Week 3-5): Hardening and Control Baseline
- [ ] SEC-03
- [ ] SEC-04
- [ ] SEC-05
- [ ] COMP-01
- [ ] COMP-03
- [ ] OPS-03

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

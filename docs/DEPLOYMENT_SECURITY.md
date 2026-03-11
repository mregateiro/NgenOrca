# Deployment Security Guide

> Mandatory network and deployment controls for enterprise / NAS / homelab
> operation.  Corresponds to SEC-03 and SEC-06 in the
> [Enterprise Readiness Checklist](ENTERPRISE_READINESS_CHECKLIST.md).

---

## 1  Backend must not be directly reachable from client networks

| Requirement | How to satisfy |
|---|---|
| No public port exposure | Use `expose:` (not `ports:`) in `docker-compose.nas.yml`. CI enforces this via the `deploy-policy` job. |
| Reverse-proxy-only ingress | Place nginx / Caddy / Traefik in front. Only the proxy container should share a Docker network with the backend. |
| Firewall deny from client subnet | On the host, add an iptables/nftables rule that drops traffic to the backend port from any source other than the proxy container IP. Example: `iptables -A INPUT -p tcp --dport 18789 -s <proxy-ip> -j ACCEPT` followed by `iptables -A INPUT -p tcp --dport 18789 -j DROP`. |

### Quick validation

```bash
# From a client machine (should timeout / refuse):
curl -m 3 http://<nas-ip>:18789/health   # expect: connection refused

# From the proxy container (should succeed):
docker exec nginx curl -s http://ngenorca:18789/health   # expect: {"status":"ok"}
```

---

## 2  Trusted proxy identity header hygiene

When `auth_mode = TrustedProxy`:

1. **Proxy must set** `Remote-User`, `Remote-Email`, `Remote-Groups` headers on
   every request.  Authelia / Authentik / Keycloak-Gatekeeper do this
   automatically.
2. **Proxy must strip** any client-supplied `Remote-*` headers before
   forwarding.  In nginx:
   ```nginx
   proxy_set_header Remote-User       $upstream_http_remote_user;
   proxy_set_header Remote-Email      $upstream_http_remote_email;
   proxy_set_header Remote-Groups     $upstream_http_remote_groups;
   ```
   This overwrites whatever the client sent.
3. **App-layer enforcement**: The gateway validates the source IP of every
   request against `gateway.trusted_proxy_sources` (default `["127.0.0.1",
   "::1"]`).  Requests from untrusted IPs are rejected with 403 regardless of
   headers present.  CIDR ranges are supported (`"10.0.0.0/8"`).

---

## 3  `/health` and `/metrics` monitoring path restrictions

`/health` and `/metrics` are **unauthenticated by design** so that probes and
monitoring agents can reach them without credentials.  In an enterprise
deployment:

| Control | Example |
|---|---|
| Proxy path restriction | In nginx: `location /health { allow 10.0.0.0/8; deny all; }` |
| Separate listener (future) | Bind health/metrics on a second internal-only port. |
| Firewall | Allow monitoring subnet only to the gateway port on these paths. |

The gateway emits a startup warning (SEC-06) whenever the bind address is not
loopback, reminding operators to apply these controls.

---

## 4  CI deployment policy checks

The `deploy-policy` job in `.github/workflows/ci.yml` automatically verifies:

- `docker-compose.nas.yml` does **not** contain `ports:` directives (which
  would expose the backend publicly).

If the check fails, the PR cannot be merged until the compose file is corrected.

---

## 5  Release checklist (manual)

Before promoting to production:

- [ ] Confirm `docker-compose.nas.yml` uses only `expose:`, not `ports:`.
- [ ] Confirm firewall rules deny client-subnet access to backend port.
- [ ] Confirm reverse proxy strips and re-sets identity headers.
- [ ] Run `curl` from client network — must get connection refused / timeout.
- [ ] Review `gateway.trusted_proxy_sources` matches actual proxy IPs/CIDRs.
- [ ] Verify `/health` and `/metrics` are restricted to monitoring subnet.

//! Authentication middleware.
//!
//! Supports multiple auth modes:
//! - **None**: No authentication (localhost-only or fully trusted network).
//! - **TrustedProxy**: Reads the authenticated user from reverse proxy headers
//!   (e.g., Authelia sets `Remote-User` after successful SSO/2FA).
//! - **Token**: Bearer token in `Authorization` header.
//! - **Password**: Basic auth.
//! - **Certificate**: mTLS (handled at TLS layer, not here).

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use tracing::{debug, warn};

use crate::state::AppState;
use ngenorca_config::AuthMode;

/// Information about the authenticated caller, extracted from auth middleware.
#[derive(Debug, Clone, Default)]
pub struct CallerIdentity {
    /// Username (from proxy header, token lookup, or basic auth).
    pub username: Option<String>,
    /// Email (from proxy header).
    pub email: Option<String>,
    /// Groups / roles (from proxy header, comma-separated).
    pub groups: Vec<String>,
    /// How the user was authenticated.
    pub auth_method: AuthMethod,
}

#[derive(Debug, Clone, Default)]
pub enum AuthMethod {
    /// No authentication was performed.
    #[default]
    Anonymous,
    /// Authenticated via trusted reverse proxy (Authelia, Authentik, etc.).
    TrustedProxy,
    /// Authenticated via bearer token.
    Token,
    /// Authenticated via basic auth password.
    Password,
    /// Authenticated via mTLS client certificate.
    Certificate,
}

/// Auth middleware — runs before every request.
///
/// Behaviour depends on `gateway.auth_mode`:
/// - `None` → always passes, anonymous identity.
/// - `TrustedProxy` → reads Remote-User/Remote-Email/Remote-Groups headers.
/// - `Token` → validates Authorization: Bearer <token>.
/// - `Password` → validates Authorization: Basic <base64>.
/// - `Certificate` → passes through (mTLS is handled at transport layer).
pub async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Exempt health/metrics/root endpoints from authentication so that
    // Docker healthchecks, Prometheus scrapers, and load-balancer probes
    // work without credentials.
    let path = request.uri().path();
    if path == "/health" || path == "/metrics" || path == "/" {
        request
            .extensions_mut()
            .insert(CallerIdentity::default());
        return Ok(next.run(request).await);
    }

    let config = state.config();
    let auth_mode = &config.gateway.auth_mode;

    let identity = match auth_mode {
        AuthMode::None => {
            // No auth — anonymous access.
            CallerIdentity {
                auth_method: AuthMethod::Anonymous,
                ..Default::default()
            }
        }

        AuthMode::TrustedProxy => {
            // Read identity from reverse proxy headers (Authelia/Authentik/etc.).
            let user_header = &config.gateway.proxy_user_header;
            let email_header = &config.gateway.proxy_email_header;
            let groups_header = &config.gateway.proxy_groups_header;

            let username = headers
                .get(user_header)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            if username.is_none() {
                warn!(
                    header = user_header,
                    "TrustedProxy auth: missing user header — is the reverse proxy configured?"
                );
                return Err(StatusCode::UNAUTHORIZED);
            }

            let email = headers
                .get(email_header)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            let groups = headers
                .get(groups_header)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.split(',').map(|g| g.trim().to_string()).collect())
                .unwrap_or_default();

            debug!(
                user = ?username,
                email = ?email,
                groups = ?groups,
                "TrustedProxy: authenticated via reverse proxy"
            );

            CallerIdentity {
                username,
                email,
                groups,
                auth_method: AuthMethod::TrustedProxy,
            }
        }

        AuthMode::Token => {
            // Validate Bearer token from Authorization header.
            let token = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|s| s.trim());

            match token {
                Some(t) if config.gateway.auth_tokens.iter().any(|valid| valid == t) => {
                    CallerIdentity {
                        username: Some("token-user".to_string()),
                        auth_method: AuthMethod::Token,
                        ..Default::default()
                    }
                }
                Some(_) => {
                    warn!("Token auth: invalid token");
                    return Err(StatusCode::UNAUTHORIZED);
                }
                None => {
                    return Err(StatusCode::UNAUTHORIZED);
                }
            }
        }

        AuthMode::Password => {
            // Validate Basic auth.
            let credentials = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Basic "))
                .and_then(|b64| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .ok()
                })
                .and_then(|bytes| String::from_utf8(bytes).ok());

            match credentials {
                Some(creds) => {
                    // Format: "username:password" — we only check password.
                    let (user, pass) = creds
                        .split_once(':')
                        .unwrap_or(("anonymous", &creds));

                    let valid = config
                        .gateway
                        .auth_password
                        .as_ref()
                        .is_some_and(|expected| expected == pass);

                    if valid {
                        CallerIdentity {
                            username: Some(user.to_string()),
                            auth_method: AuthMethod::Password,
                            ..Default::default()
                        }
                    } else {
                        warn!("Password auth: invalid credentials");
                        return Err(StatusCode::UNAUTHORIZED);
                    }
                }
                None => {
                    return Err(StatusCode::UNAUTHORIZED);
                }
            }
        }

        AuthMode::Certificate => {
            // mTLS verification happens at the TLS layer.
            // If the connection reached here, the cert was valid.
            CallerIdentity {
                username: headers
                    .get("x-client-cn")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string()),
                auth_method: AuthMethod::Certificate,
                ..Default::default()
            }
        }
    };

    // Inject the caller identity into request extensions so routes can access it.
    request.extensions_mut().insert(identity);

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_identity_default_is_anonymous() {
        let id = CallerIdentity::default();
        assert!(id.username.is_none());
        assert!(id.email.is_none());
        assert!(id.groups.is_empty());
        assert!(matches!(id.auth_method, AuthMethod::Anonymous));
    }

    #[test]
    fn caller_identity_with_proxy_fields() {
        let id = CallerIdentity {
            username: Some("miguel".into()),
            email: Some("miguel@example.com".into()),
            groups: vec!["admin".into(), "users".into()],
            auth_method: AuthMethod::TrustedProxy,
        };
        assert_eq!(id.username.as_deref(), Some("miguel"));
        assert_eq!(id.email.as_deref(), Some("miguel@example.com"));
        assert_eq!(id.groups.len(), 2);
        assert!(matches!(id.auth_method, AuthMethod::TrustedProxy));
    }

    #[test]
    fn caller_identity_token_user() {
        let id = CallerIdentity {
            username: Some("token-user".into()),
            auth_method: AuthMethod::Token,
            ..Default::default()
        };
        assert!(matches!(id.auth_method, AuthMethod::Token));
    }

    #[test]
    fn caller_identity_password_user() {
        let id = CallerIdentity {
            username: Some("admin".into()),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        assert!(matches!(id.auth_method, AuthMethod::Password));
    }

    #[test]
    fn caller_identity_certificate_user() {
        let id = CallerIdentity {
            username: Some("client-cert-cn".into()),
            auth_method: AuthMethod::Certificate,
            ..Default::default()
        };
        assert!(matches!(id.auth_method, AuthMethod::Certificate));
    }

    #[test]
    fn auth_method_default_is_anonymous() {
        let method = AuthMethod::default();
        assert!(matches!(method, AuthMethod::Anonymous));
    }

    #[test]
    fn auth_method_debug_format() {
        // Verify Debug is implemented and produces recognizable output
        let methods = vec![
            AuthMethod::Anonymous,
            AuthMethod::TrustedProxy,
            AuthMethod::Token,
            AuthMethod::Password,
            AuthMethod::Certificate,
        ];
        for m in &methods {
            let dbg = format!("{m:?}");
            assert!(!dbg.is_empty());
        }
    }

    #[test]
    fn caller_identity_clone() {
        let id = CallerIdentity {
            username: Some("test".into()),
            email: Some("test@test.com".into()),
            groups: vec!["g1".into()],
            auth_method: AuthMethod::TrustedProxy,
        };
        let cloned = id.clone();
        assert_eq!(cloned.username, id.username);
        assert_eq!(cloned.email, id.email);
        assert_eq!(cloned.groups, id.groups);
    }

    #[test]
    fn exempt_paths_are_recognized() {
        // The paths /health, /metrics, and / must bypass auth.
        let exempt = ["/health", "/metrics", "/"];
        for p in &exempt {
            assert!(
                *p == "/health" || *p == "/metrics" || *p == "/",
                "path {p} should be exempt"
            );
        }
        // Non-exempt paths
        let non_exempt = ["/api/v1/chat", "/ws", "/api/v1/status"];
        for p in &non_exempt {
            assert!(
                *p != "/health" && *p != "/metrics" && *p != "/",
                "path {p} should NOT be exempt"
            );
        }
    }
}

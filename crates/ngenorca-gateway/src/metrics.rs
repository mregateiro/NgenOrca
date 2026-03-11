//! Lightweight Prometheus-compatible metrics.
//!
//! Tracks key operational counters and gauges for the gateway. Metrics are
//! exposed as plain-text at `GET /metrics` in Prometheus exposition format.
//! No external metrics crate is needed — we use atomics directly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared metrics registry. Clone-friendly (wraps `Arc`).
#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    // ── Request counters ────────────────────────────────────────
    /// Total HTTP requests received.
    http_requests_total: AtomicU64,
    /// Total HTTP requests that received a 4xx/5xx response.
    http_errors_total: AtomicU64,
    /// Total WebSocket connections opened.
    ws_connections_total: AtomicU64,
    /// Currently active WebSocket connections (gauge).
    ws_connections_active: AtomicU64,
    /// Total WebSocket messages received from clients.
    ws_messages_in_total: AtomicU64,

    // ── Orchestration counters ──────────────────────────────────
    /// Total orchestration cycles completed.
    orchestrations_total: AtomicU64,
    /// Total orchestration cycles that resulted in escalation.
    escalations_total: AtomicU64,
    /// Total orchestration cycles that triggered augmentation.
    augmentations_total: AtomicU64,

    // ── Rate limiter ────────────────────────────────────────────
    /// Total requests rejected by rate limiter.
    rate_limited_total: AtomicU64,

    // ── Memory ──────────────────────────────────────────────────
    /// Total memory consolidation runs.
    consolidations_total: AtomicU64,

    // ── Token usage ─────────────────────────────────────────────
    /// Cumulative token usage across all requests.
    tokens_total: AtomicU64,
}

impl Metrics {
    /// Create a new, zeroed metrics registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner {
                http_requests_total: AtomicU64::new(0),
                http_errors_total: AtomicU64::new(0),
                ws_connections_total: AtomicU64::new(0),
                ws_connections_active: AtomicU64::new(0),
                ws_messages_in_total: AtomicU64::new(0),
                orchestrations_total: AtomicU64::new(0),
                escalations_total: AtomicU64::new(0),
                augmentations_total: AtomicU64::new(0),
                rate_limited_total: AtomicU64::new(0),
                consolidations_total: AtomicU64::new(0),
                tokens_total: AtomicU64::new(0),
            }),
        }
    }

    // ── Increment helpers ───────────────────────────────────────

    pub fn inc_http_requests(&self) {
        self.inner.http_requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_http_errors(&self) {
        self.inner.http_errors_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_ws_connections(&self) {
        self.inner.ws_connections_total.fetch_add(1, Ordering::Relaxed);
        self.inner.ws_connections_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the active WS connection gauge (called on disconnect).
    pub fn dec_ws_connections(&self) {
        self.inner.ws_connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// Current number of active WS connections.
    pub fn ws_connections_active(&self) -> u64 {
        self.inner.ws_connections_active.load(Ordering::Relaxed)
    }

    pub fn inc_ws_messages_in(&self) {
        self.inner.ws_messages_in_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_orchestrations(&self) {
        self.inner.orchestrations_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_escalations(&self) {
        self.inner.escalations_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_augmentations(&self) {
        self.inner.augmentations_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rate_limited(&self) {
        self.inner.rate_limited_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_consolidations(&self) {
        self.inner.consolidations_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_tokens(&self, n: u64) {
        self.inner.tokens_total.fetch_add(n, Ordering::Relaxed);
    }

    // ── Snapshot (for the /metrics endpoint) ────────────────────

    /// Render all metrics in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        let i = &self.inner;
        let mut buf = String::with_capacity(1024);

        prom_counter(
            &mut buf,
            "ngenorca_http_requests_total",
            "Total HTTP requests received",
            i.http_requests_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_http_errors_total",
            "Total HTTP error responses (4xx/5xx)",
            i.http_errors_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_ws_connections_total",
            "Total WebSocket connections opened",
            i.ws_connections_total.load(Ordering::Relaxed),
        );
        prom_gauge(
            &mut buf,
            "ngenorca_ws_connections_active",
            "Currently active WebSocket connections",
            i.ws_connections_active.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_ws_messages_in_total",
            "Total WebSocket messages received from clients",
            i.ws_messages_in_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_orchestrations_total",
            "Total orchestration cycles completed",
            i.orchestrations_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_escalations_total",
            "Total orchestrations that escalated to a larger model",
            i.escalations_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_augmentations_total",
            "Total orchestrations that required augmentation",
            i.augmentations_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_rate_limited_total",
            "Total requests rejected by rate limiter",
            i.rate_limited_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_consolidations_total",
            "Total memory consolidation runs",
            i.consolidations_total.load(Ordering::Relaxed),
        );
        prom_counter(
            &mut buf,
            "ngenorca_tokens_total",
            "Cumulative token usage across all requests",
            i.tokens_total.load(Ordering::Relaxed),
        );

        buf
    }

    // ── Getters (for tests / diagnostics) ───────────────────────

    pub fn http_requests(&self) -> u64 {
        self.inner.http_requests_total.load(Ordering::Relaxed)
    }

    pub fn orchestrations(&self) -> u64 {
        self.inner.orchestrations_total.load(Ordering::Relaxed)
    }

    pub fn tokens(&self) -> u64 {
        self.inner.tokens_total.load(Ordering::Relaxed)
    }

    pub fn rate_limited(&self) -> u64 {
        self.inner.rate_limited_total.load(Ordering::Relaxed)
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Write a single Prometheus counter line.
fn prom_counter(buf: &mut String, name: &str, help: &str, value: u64) {
    buf.push_str("# HELP ");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(help);
    buf.push('\n');
    buf.push_str("# TYPE ");
    buf.push_str(name);
    buf.push_str(" counter\n");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(&value.to_string());
    buf.push('\n');
}

/// Write a single Prometheus gauge line.
fn prom_gauge(buf: &mut String, name: &str, help: &str, value: u64) {
    buf.push_str("# HELP ");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(help);
    buf.push('\n');
    buf.push_str("# TYPE ");
    buf.push_str(name);
    buf.push_str(" gauge\n");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(&value.to_string());
    buf.push('\n');
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_all_zero() {
        let m = Metrics::new();
        assert_eq!(m.http_requests(), 0);
        assert_eq!(m.orchestrations(), 0);
        assert_eq!(m.tokens(), 0);
        assert_eq!(m.rate_limited(), 0);
    }

    #[test]
    fn increments_are_visible() {
        let m = Metrics::new();
        m.inc_http_requests();
        m.inc_http_requests();
        m.inc_orchestrations();
        m.add_tokens(150);
        m.add_tokens(50);
        assert_eq!(m.http_requests(), 2);
        assert_eq!(m.orchestrations(), 1);
        assert_eq!(m.tokens(), 200);
    }

    #[test]
    fn clone_shares_state() {
        let m1 = Metrics::new();
        let m2 = m1.clone();
        m1.inc_http_requests();
        assert_eq!(m2.http_requests(), 1);
    }

    #[test]
    fn prometheus_output_format() {
        let m = Metrics::new();
        m.inc_http_requests();
        m.inc_http_requests();
        m.inc_rate_limited();
        let output = m.render_prometheus();
        assert!(output.contains("# HELP ngenorca_http_requests_total"));
        assert!(output.contains("# TYPE ngenorca_http_requests_total counter"));
        assert!(output.contains("ngenorca_http_requests_total 2"));
        assert!(output.contains("ngenorca_rate_limited_total 1"));
        assert!(output.contains("ngenorca_tokens_total 0"));
    }
}

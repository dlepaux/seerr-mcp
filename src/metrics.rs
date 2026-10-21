//! The seven counters. Each (counter, kind) series exists from startup at 0, so the first
//! failure is an `increase()` the alert rules can see, not a series that appears already at 1.

use prometheus::{Encoder, IntCounterVec, Opts, Registry, TextEncoder};

pub const SEERR_FAILURES: &str = "seerr_mcp_seerr_failures_total";
pub const REQUEST_FAILURES: &str = "seerr_mcp_request_failures_total";
pub const AUTH_FAILURES: &str = "seerr_mcp_auth_failures_total";
pub const NOTIFY_FAILURES: &str = "seerr_mcp_notify_failures_total";
pub const REQUEST_SKIPPED: &str = "seerr_mcp_request_skipped_total";
pub const NOTIFY_SKIPPED: &str = "seerr_mcp_notify_skipped_total";
pub const LANGUAGE_SKIPPED: &str = "seerr_mcp_language_skipped_total";

/// Every counter and its kinds. The `_failures_total` four are faults worth an alert; the request and
/// notify `_skipped_total` two are user outcomes, worth a dashboard at most. `language_skipped` is a
/// person answered in their profile language instead of the one the model named: rare, and each one
/// is a language this build could add.
const COUNTERS: [(&str, &str, &[&str]); 7] = [
    (
        SEERR_FAILURES,
        "Seerr calls that failed",
        // `unexpected` catches the rest: a status or body the client does not know would
        // otherwise be a failure no counter sees.
        &["unreachable", "http_5xx", "permission_denied", "unexpected"],
    ),
    (
        REQUEST_FAILURES,
        "request_media calls that could not ask or answer",
        &["elicitation_unsupported", "timeout"],
    ),
    (
        AUTH_FAILURES,
        "Refused credentials on /mcp or /notify",
        &["mcp_token", "notify_secret"],
    ),
    (
        NOTIFY_FAILURES,
        "Ready notifications not delivered",
        &["hermes_rejected", "hermes_unreachable", "bad_payload"],
    ),
    (
        REQUEST_SKIPPED,
        "request_media calls that filed nothing, by user outcome",
        &["declined", "quota", "duplicate", "already_available"],
    ),
    (
        NOTIFY_SKIPPED,
        "Ready notifications for a user with no chat",
        &["unmapped_user"],
    ),
    (
        LANGUAGE_SKIPPED,
        "Tool calls whose named language this build has no locale for, so the profile's was used",
        &["unsupported_language"],
    ),
];

pub struct Metrics {
    registry: Registry,
    counters: Vec<(&'static str, IntCounterVec)>,
}

impl Metrics {
    #[must_use]
    pub fn new() -> Self {
        let registry = Registry::new();
        let counters = COUNTERS
            .iter()
            .map(|(name, help, kinds)| {
                let vec = IntCounterVec::new(Opts::new(*name, *help), &["kind"])
                    .expect("static counter definition is valid");
                registry
                    .register(Box::new(vec.clone()))
                    .expect("each counter is registered once");
                for kind in *kinds {
                    vec.with_label_values(&[*kind]);
                }
                (*name, vec)
            })
            .collect();
        Self { registry, counters }
    }

    /// `kind` must be one of the counter's declared kinds: an undeclared one would make a series that
    /// no alert and no pre-registration knows about. Debug builds and tests panic on it.
    pub fn inc(&self, counter: &str, kind: &str) {
        debug_assert!(
            COUNTERS
                .iter()
                .any(|(n, _, kinds)| *n == counter && kinds.contains(&kind)),
            "undeclared counter kind {counter}{{{kind}}}"
        );
        if let Some((_, vec)) = self.counters.iter().find(|(n, _)| *n == counter) {
            vec.with_label_values(&[kind]).inc();
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut out)
            .expect("text encoding of counters cannot fail");
        String::from_utf8(out).expect("the text format is UTF-8")
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(render: &str, name: &str, kind: &str) -> Option<String> {
        let prefix = format!("{name}{{kind=\"{kind}\"}} ");
        render
            .lines()
            .find(|l| l.starts_with(&prefix))
            .map(|l| l[prefix.len()..].to_string())
    }

    #[test]
    fn exactly_the_spec_counters_and_kinds_start_at_zero() {
        let expected: [(&str, &[&str]); 7] = [
            (
                "seerr_mcp_seerr_failures_total",
                &["unreachable", "http_5xx", "permission_denied", "unexpected"],
            ),
            (
                "seerr_mcp_request_failures_total",
                &["elicitation_unsupported", "timeout"],
            ),
            (
                "seerr_mcp_auth_failures_total",
                &["mcp_token", "notify_secret"],
            ),
            (
                "seerr_mcp_notify_failures_total",
                &["hermes_rejected", "hermes_unreachable", "bad_payload"],
            ),
            (
                "seerr_mcp_request_skipped_total",
                &["declined", "quota", "duplicate", "already_available"],
            ),
            ("seerr_mcp_notify_skipped_total", &["unmapped_user"]),
            (
                "seerr_mcp_language_skipped_total",
                &["unsupported_language"],
            ),
        ];
        let render = Metrics::new().render();
        let mut series = 0;
        for (name, kinds) in expected {
            for kind in kinds {
                assert_eq!(
                    line(&render, name, kind).as_deref(),
                    Some("0"),
                    "{name}{{{kind}}}"
                );
                series += 1;
            }
        }
        let samples = render.lines().filter(|l| !l.starts_with('#')).count();
        assert_eq!(samples, series, "no series beyond the table:\n{render}");
    }

    #[test]
    fn inc_counts_its_own_kind_only() {
        let metrics = Metrics::new();
        metrics.inc(SEERR_FAILURES, "http_5xx");
        let render = metrics.render();
        assert_eq!(
            line(&render, SEERR_FAILURES, "http_5xx").as_deref(),
            Some("1")
        );
        assert_eq!(
            line(&render, SEERR_FAILURES, "unreachable").as_deref(),
            Some("0")
        );
    }
}

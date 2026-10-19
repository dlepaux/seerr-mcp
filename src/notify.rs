//! `/notify`: Seerr's "Media Available" webhook in, one message per mapped person out, POSTed to that
//! person's Hermes `deliver_only` route (`ready-<profile>`) and signed with Hermes's generic HMAC V2.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::config::{normalise_jellyfin_id, Identity, Language};
use crate::metrics::{Metrics, AUTH_FAILURES, NOTIFY_FAILURES, NOTIFY_SKIPPED};

const MAX_SUBJECT: usize = 120;
const HERMES_TIMEOUT: Duration = Duration::from_secs(10);

/// Seerr's webhook template sends every field as a string; only these three are read.
#[derive(Deserialize)]
struct Payload {
    notification_type: String,
    subject: String,
    #[serde(rename = "requestedBy_jellyfinUserId")]
    requested_by_jellyfin_user_id: String,
}

/// Hermes's V2 signature: hex HMAC-SHA256 over `<unix seconds>.<body>`.
#[must_use]
pub fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("{timestamp}.").as_bytes());
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn message(language: Language, subject: &str) -> String {
    let subject: String = subject
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_SUBJECT)
        .collect();
    match language {
        Language::Fr => format!("🎬 « {subject} » est prêt sur Jellyfin."),
        Language::En => format!("🎬 \"{subject}\" is ready on Jellyfin."),
    }
}

pub struct Notifier {
    http: reqwest::Client,
    secret: String,
    hermes_base: String,
    hermes_secret: String,
    identities: Vec<Identity>,
}

impl Notifier {
    /// # Panics
    /// If the TLS backend cannot initialise, a build defect.
    #[must_use]
    pub fn new(
        secret: &str,
        hermes_base: &str,
        hermes_secret: &str,
        identities: Vec<Identity>,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(HERMES_TIMEOUT)
                .build()
                .expect("the rustls client builds"),
            secret: secret.to_string(),
            hermes_base: hermes_base.trim_end_matches('/').to_string(),
            hermes_secret: hermes_secret.to_string(),
            identities,
        }
    }

    /// The HTTP status to answer Seerr with: 401 bad secret, 400 bad payload, 204 nothing to send
    /// (another type, or nobody with a chat), 200 delivered, 502 Hermes refused or unreachable.
    pub async fn notify(&self, metrics: &Metrics, secret: Option<&str>, body: &[u8]) -> u16 {
        let given = secret.unwrap_or_default().as_bytes();
        if given.is_empty() || !bool::from(given.ct_eq(self.secret.as_bytes())) {
            metrics.inc(AUTH_FAILURES, "notify_secret");
            tracing::warn!("/notify: refused a request with a wrong or missing secret");
            return 401;
        }
        let Ok(payload) = serde_json::from_slice::<Payload>(body) else {
            metrics.inc(NOTIFY_FAILURES, "bad_payload");
            tracing::warn!("/notify: payload is not the expected webhook JSON");
            return 400;
        };
        if payload.notification_type != "MEDIA_AVAILABLE" {
            return 204;
        }
        let jellyfin = normalise_jellyfin_id(&payload.requested_by_jellyfin_user_id);
        // The room is Hermes's business; seerr-mcp only needs to know there is one.
        let Some(who) = self
            .identities
            .iter()
            .find(|i| i.jellyfin_user_id == jellyfin && i.matrix_room_id.is_some())
        else {
            metrics.inc(NOTIFY_SKIPPED, "unmapped_user");
            return 204;
        };

        let out = serde_json::to_vec(&serde_json::json!({
            "message": message(who.language, &payload.subject)
        }))
        .expect("a JSON object of one string serialises");
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_secs();
        let sent = self
            .http
            // One route per person: Hermes reads routes only from its default profile, and a route binds
            // exactly one profile, so a shared "ready" route could serve one person, never two.
            .post(format!(
                "{}/p/{p}/webhooks/ready-{p}",
                self.hermes_base,
                p = who.profile
            ))
            .header("Content-Type", "application/json")
            .header("X-Webhook-Timestamp", timestamp.to_string())
            .header(
                "X-Webhook-Signature-V2",
                sign(&self.hermes_secret, timestamp, &out),
            )
            .body(out)
            .send()
            .await;
        match sent {
            Ok(resp) if resp.status().is_success() => 200,
            Ok(resp) => {
                metrics.inc(NOTIFY_FAILURES, "hermes_rejected");
                tracing::warn!(status = %resp.status(), profile = %who.profile, "/notify: Hermes refused the message");
                502
            }
            Err(e) => {
                metrics.inc(NOTIFY_FAILURES, "hermes_unreachable");
                tracing::warn!(error = %e, profile = %who.profile, "/notify: Hermes unreachable");
                502
            }
        }
    }
}

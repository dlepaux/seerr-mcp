//! `/notify` against a wiremock Hermes: Seerr's webhook in, one signed `deliver_only` POST out.
//! The signature check mirrors Hermes's `_validate_signature` V2 (gateway/platforms/webhook.py at
//! 8f817911): hex HMAC-SHA256 over `<timestamp>.<body>`.

use std::num::NonZeroU32;

use hmac::{Hmac, Mac};
use seerr_mcp::config::{Identity, Language, SeerrUserId};
use seerr_mcp::metrics::Metrics;
use seerr_mcp::notify::{self, Notifier};
use serde_json::{json, Value};
use sha2::Sha256;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "notify-secret";
const HERMES_SECRET: &str = "hermes-secret";

fn person(profile: &str, jellyfin: &str, room: Option<&str>, language: Language) -> Identity {
    Identity {
        profile: profile.into(),
        token_sha256: [0; 32],
        seerr_user_id: SeerrUserId::new(NonZeroU32::new(7).unwrap()),
        jellyfin_user_id: jellyfin.into(),
        matrix_room_id: room.map(str::to_string),
        language,
    }
}

fn household() -> Vec<Identity> {
    vec![
        person("alice", "bb11cc22", Some("!n:m"), Language::Fr),
        person("bob", "aa00", Some("!d:m"), Language::En),
        person("guest", "ee55", None, Language::En),
    ]
}

fn available(jellyfin: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "notification_type": "MEDIA_AVAILABLE",
        "subject": "Paddington au Pérou (2024)",
        "media_type": "movie",
        "media_tmdbid": "1022789",
        "requestedBy_jellyfinUserId": jellyfin,
        "request_id": "42",
    }))
    .unwrap()
}

fn counter(metrics: &Metrics, name: &str, kind: &str) -> String {
    let prefix = format!("{name}{{kind=\"{kind}\"}} ");
    metrics
        .render()
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
        .unwrap()
}

async fn hermes(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    server
}

fn notifier(base: &str) -> Notifier {
    Notifier::new(SECRET, base, HERMES_SECRET, household())
}

async fn deliveries(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap()
}

#[tokio::test]
async fn a_mapped_user_gets_one_signed_message_in_their_language() {
    let server = hermes(200).await;
    let metrics = Metrics::new();
    // Seerr sends the Jellyfin id as a dashed GUID in any case; the map holds it normalised.
    let status = notifier(&server.uri())
        .notify(&metrics, Some(SECRET), &available("BB11-CC22"))
        .await;
    assert_eq!(status, 200);

    let got = deliveries(&server).await;
    assert_eq!(got.len(), 1);
    let r = &got[0];
    assert_eq!(r.url.path(), "/p/alice/webhooks/ready-alice");
    let body: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(
        body,
        json!({"message": "🎬 « Paddington au Pérou (2024) » est prêt sur Jellyfin."})
    );

    let ts = r
        .headers
        .get("x-webhook-timestamp")
        .unwrap()
        .to_str()
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(ts.parse::<u64>().unwrap()) < 5, "unix seconds");
    let mut mac = Hmac::<Sha256>::new_from_slice(HERMES_SECRET.as_bytes()).unwrap();
    mac.update(format!("{ts}.").as_bytes());
    mac.update(&r.body);
    assert_eq!(
        r.headers
            .get("x-webhook-signature-v2")
            .unwrap()
            .to_str()
            .unwrap(),
        hex::encode(mac.finalize().into_bytes())
    );
    assert_eq!(r.headers.get("content-type").unwrap(), "application/json");
}

#[tokio::test]
async fn english_speakers_get_english() {
    let server = hermes(200).await;
    let status = notifier(&server.uri())
        .notify(&Metrics::new(), Some(SECRET), &available("aa00"))
        .await;
    assert_eq!(status, 200);
    let got = deliveries(&server).await;
    assert_eq!(got[0].url.path(), "/p/bob/webhooks/ready-bob");
    let body: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(
        body,
        json!({"message": "🎬 \"Paddington au Pérou (2024)\" is ready on Jellyfin."})
    );
}

#[tokio::test]
async fn an_unmapped_user_or_one_without_a_room_is_skipped_and_counted() {
    let server = hermes(200).await;
    let metrics = Metrics::new();
    let n = notifier(&server.uri());
    assert_eq!(
        n.notify(&metrics, Some(SECRET), &available("ff99")).await,
        204
    );
    assert_eq!(
        n.notify(&metrics, Some(SECRET), &available("ee55")).await,
        204
    );
    assert!(deliveries(&server).await.is_empty());
    assert_eq!(
        counter(&metrics, "seerr_mcp_notify_skipped_total", "unmapped_user"),
        "2"
    );
}

#[tokio::test]
async fn a_wrong_or_missing_secret_is_refused_and_counted() {
    let server = hermes(200).await;
    let metrics = Metrics::new();
    let n = notifier(&server.uri());
    assert_eq!(
        n.notify(&metrics, Some("nope"), &available("aa00")).await,
        401
    );
    assert_eq!(n.notify(&metrics, None, &available("aa00")).await, 401);
    assert_eq!(n.notify(&metrics, Some(""), &available("aa00")).await, 401);
    assert!(deliveries(&server).await.is_empty());
    assert_eq!(
        counter(&metrics, "seerr_mcp_auth_failures_total", "notify_secret"),
        "3"
    );
}

#[tokio::test]
async fn other_notification_types_are_ignored() {
    let server = hermes(200).await;
    let metrics = Metrics::new();
    let body = serde_json::to_vec(&json!({
        "notification_type": "TEST_NOTIFICATION", "subject": "Test", "requestedBy_jellyfinUserId": "aa00"
    }))
    .unwrap();
    assert_eq!(
        notifier(&server.uri())
            .notify(&metrics, Some(SECRET), &body)
            .await,
        204
    );
    assert!(deliveries(&server).await.is_empty());
}

#[tokio::test]
async fn a_malformed_payload_is_400_and_counted() {
    let server = hermes(200).await;
    let metrics = Metrics::new();
    let n = notifier(&server.uri());
    assert_eq!(n.notify(&metrics, Some(SECRET), b"not json").await, 400);
    let missing =
        serde_json::to_vec(&json!({"notification_type": "MEDIA_AVAILABLE", "subject": "x"}))
            .unwrap();
    assert_eq!(n.notify(&metrics, Some(SECRET), &missing).await, 400);
    assert_eq!(
        counter(&metrics, "seerr_mcp_notify_failures_total", "bad_payload"),
        "2"
    );
}

#[tokio::test]
async fn the_subject_is_untrusted() {
    let server = hermes(200).await;
    let body = serde_json::to_vec(&json!({
        "notification_type": "MEDIA_AVAILABLE",
        "subject": format!("Bad\u{1b}]0;x\u{7}{}", "y".repeat(500)),
        "requestedBy_jellyfinUserId": "aa00",
    }))
    .unwrap();
    notifier(&server.uri())
        .notify(&Metrics::new(), Some(SECRET), &body)
        .await;
    let got = deliveries(&server).await;
    let message = serde_json::from_slice::<Value>(&got[0].body).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!message.chars().any(char::is_control), "{message:?}");
    assert!(message.chars().count() < 200, "{}", message.chars().count());
}

#[tokio::test]
async fn hermes_refusing_is_502_and_counted() {
    let server = hermes(502).await;
    let metrics = Metrics::new();
    assert_eq!(
        notifier(&server.uri())
            .notify(&metrics, Some(SECRET), &available("aa00"))
            .await,
        502
    );
    assert_eq!(
        counter(
            &metrics,
            "seerr_mcp_notify_failures_total",
            "hermes_rejected"
        ),
        "1"
    );
}

#[tokio::test]
async fn hermes_down_is_502_and_counted() {
    let metrics = Metrics::new();
    let status = notifier("http://127.0.0.1:9")
        .notify(&metrics, Some(SECRET), &available("aa00"))
        .await;
    assert_eq!(status, 502);
    assert_eq!(
        counter(
            &metrics,
            "seerr_mcp_notify_failures_total",
            "hermes_unreachable"
        ),
        "1"
    );
}

#[test]
fn the_signature_is_hex_hmac_over_timestamp_dot_body() {
    // Fixed vector, computed independently: python3 -c "import hmac,hashlib;
    // print(hmac.new(b'k', b'1700000000.{}', hashlib.sha256).hexdigest())"
    assert_eq!(
        notify::sign("k", 1_700_000_000, b"{}"),
        "c6ef4734033cee2367414a0939385eca0397754aa7a59716da7463a9298b5146"
    );
}

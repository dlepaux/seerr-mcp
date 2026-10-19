//! The listener: which routes need what, and the whole tool path end to end through rmcp's own client
//! (Streamable HTTP, bearer, form elicitation) against a wiremock Seerr.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationCapability, FormElicitationCapability, Implementation,
};
use rmcp::service::{RequestContext, RoleClient};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{ClientHandler, ErrorData, ServiceExt};
use seerr_mcp::config::{Identity, Language, SeerrUserId};
use seerr_mcp::http::{build_router, App};
use seerr_mcp::metrics::Metrics;
use seerr_mcp::notify::Notifier;
use seerr_mcp::seerr::Seerr;
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ALICE_TOKEN: &str = "token-alice";
const BOB_TOKEN: &str = "token-bob";

fn person(profile: &str, token: &str, seerr_id: u32, jellyfin: &str) -> Identity {
    Identity {
        profile: profile.into(),
        token_sha256: Sha256::digest(token.as_bytes()).into(),
        seerr_user_id: SeerrUserId::new(NonZeroU32::new(seerr_id).unwrap()),
        jellyfin_user_id: jellyfin.into(),
        matrix_room_id: None,
        language: Language::Fr,
    }
}

fn app(seerr_url: &str) -> Arc<App> {
    let identities = vec![
        person("alice", ALICE_TOKEN, 7, "bb11"),
        person("bob", BOB_TOKEN, 1, "aa00"),
    ];
    Arc::new(App {
        seerr: Seerr::new(seerr_url, "admin-key", Duration::from_secs(5)),
        metrics: Metrics::new(),
        notifier: Notifier::new(
            "notify-secret",
            "http://127.0.0.1:9",
            "h",
            identities.clone(),
        ),
        identities,
    })
}

async fn serve(app: Arc<App>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, build_router(app)).await.unwrap() });
    format!("http://{addr}")
}

fn counter(app: &App, name: &str, kind: &str) -> String {
    let prefix = format!("{name}{{kind=\"{kind}\"}} ");
    app.metrics
        .render()
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
        .unwrap()
}

#[tokio::test]
async fn health_and_metrics_need_no_token() {
    let base = serve(app("http://127.0.0.1:9")).await;
    let health = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(health.status(), 200);
    let metrics = reqwest::get(format!("{base}/metrics")).await.unwrap();
    assert_eq!(metrics.status(), 200);
    assert!(metrics
        .text()
        .await
        .unwrap()
        .contains("seerr_mcp_auth_failures_total"));
}

#[tokio::test]
async fn mcp_refuses_a_missing_or_unknown_token_and_counts_it() {
    let app = app("http://127.0.0.1:9");
    let base = serve(app.clone()).await;
    let client = reqwest::Client::new();
    let none = client.post(format!("{base}/mcp")).send().await.unwrap();
    assert_eq!(none.status(), 401);
    let wrong = client
        .post(format!("{base}/mcp"))
        .bearer_auth("not-a-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let scheme = client
        .post(format!("{base}/mcp"))
        .header("Authorization", ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(scheme.status(), 401);
    assert_eq!(
        counter(&app, "seerr_mcp_auth_failures_total", "mcp_token"),
        "3"
    );
}

#[tokio::test]
async fn notify_takes_its_secret_header() {
    let app = app("http://127.0.0.1:9");
    let base = serve(app.clone()).await;
    let client = reqwest::Client::new();
    let body = json!({"notification_type": "TEST_NOTIFICATION", "subject": "x",
                      "requestedBy_jellyfinUserId": "aa00"});
    let bad = client
        .post(format!("{base}/notify"))
        .header("X-Seerr-Mcp-Secret", "nope")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    let good = client
        .post(format!("{base}/notify"))
        .header("X-Seerr-Mcp-Secret", "notify-secret")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(good.status(), 204);
    assert_eq!(
        counter(&app, "seerr_mcp_auth_failures_total", "notify_secret"),
        "1"
    );
}

/// A chat client that answers every confirmation with `answer`, or declares no elicitation at all.
#[derive(Clone)]
struct Chat {
    answer: Option<ElicitationAction>,
    asked: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ClientHandler for Chat {
    fn get_info(&self) -> ClientConfig {
        let mut capabilities = ClientCapabilities::default();
        if self.answer.is_some() {
            capabilities.elicitation =
                Some(ElicitationCapability::new().with_form(FormElicitationCapability::new()));
        }
        ClientConfig::new(capabilities, Implementation::new("test-chat", "0"))
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        if let ElicitRequestParams::FormElicitationParams { message, .. } = request {
            self.asked.lock().unwrap().push(message);
        }
        Ok(ElicitResult::new(
            self.answer.clone().expect("declared elicitation"),
        ))
    }
}

async fn call(
    base: &str,
    token: &str,
    chat: Chat,
    tool: &'static str,
    args: serde_json::Value,
) -> rmcp::model::CallToolResult {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")).auth_header(token),
    );
    let client = chat.serve(transport).await.expect("initialize");
    let result = client
        .call_tool(
            CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .expect("tool call");
    client.cancel().await.ok();
    result
}

fn text(result: &rmcp::model::CallToolResult) -> String {
    serde_json::to_string(&result.content).unwrap()
}

async fn paddington_seerr() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/movie/1022789"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"title": "Paddington au Pérou", "releaseDate": "2024-11-08"}),
            ),
        )
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn end_to_end_an_accepted_request_is_filed_as_the_token_holder() {
    let seerr = paddington_seerr().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/request"))
        .and(header("x-api-user", "7"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&seerr)
        .await;
    let base = serve(app(&seerr.uri())).await;
    let chat = Chat {
        answer: Some(ElicitationAction::Accept),
        asked: Arc::default(),
    };
    let result = call(
        &base,
        ALICE_TOKEN,
        chat.clone(),
        "request_media",
        json!({"tmdb_id": 1022789, "type": "movie"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{}", text(&result));
    assert!(text(&result).contains("Requested"), "{}", text(&result));
    assert_eq!(chat.asked.lock().unwrap().len(), 1);
    for r in seerr.received_requests().await.unwrap() {
        assert_eq!(r.headers.get("x-api-user").unwrap(), "7");
    }
}

#[tokio::test]
async fn end_to_end_another_token_acts_as_another_user() {
    let seerr = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/1/requests"))
        .and(header("x-api-user", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .expect(1)
        .mount(&seerr)
        .await;
    let base = serve(app(&seerr.uri())).await;
    let chat = Chat {
        answer: Some(ElicitationAction::Accept),
        asked: Arc::default(),
    };
    let result = call(&base, BOB_TOKEN, chat, "my_requests", json!({})).await;
    assert!(
        text(&result).contains("No requests yet"),
        "{}",
        text(&result)
    );
}

#[tokio::test]
async fn end_to_end_a_decline_files_nothing() {
    let seerr = paddington_seerr().await;
    let base = serve(app(&seerr.uri())).await;
    let chat = Chat {
        answer: Some(ElicitationAction::Decline),
        asked: Arc::default(),
    };
    let result = call(
        &base,
        ALICE_TOKEN,
        chat,
        "request_media",
        json!({"tmdb_id": 1022789, "type": "movie"}),
    )
    .await;
    assert!(text(&result).contains("Pas demandé"), "{}", text(&result));
    let posts = seerr.received_requests().await.unwrap();
    assert!(posts.iter().all(|r| r.method.as_str() != "POST"));
}

#[tokio::test]
async fn end_to_end_a_client_without_elicitation_files_nothing() {
    let seerr = paddington_seerr().await;
    let app = app(&seerr.uri());
    let base = serve(app.clone()).await;
    let chat = Chat {
        answer: None,
        asked: Arc::default(),
    };
    let result = call(
        &base,
        ALICE_TOKEN,
        chat,
        "request_media",
        json!({"tmdb_id": 1022789, "type": "movie"}),
    )
    .await;
    assert!(
        text(&result).contains("cannot ask for a confirmation"),
        "{}",
        text(&result)
    );
    assert!(seerr.received_requests().await.unwrap().is_empty());
    assert_eq!(
        counter(
            &app,
            "seerr_mcp_request_failures_total",
            "elicitation_unsupported"
        ),
        "1"
    );
}

#[tokio::test]
async fn the_tools_are_listed_with_their_hints() {
    let base = serve(app("http://127.0.0.1:9")).await;
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp"))
            .auth_header(ALICE_TOKEN),
    );
    let chat = Chat {
        answer: None,
        asked: Arc::default(),
    };
    let client = chat.serve(transport).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["my_requests", "request_media", "search_media"]);
    for t in &tools {
        let read_only = t.annotations.as_ref().and_then(|a| a.read_only_hint);
        let expected = (t.name != "request_media").then_some(true);
        assert_eq!(read_only, expected, "{}", t.name);
    }
    client.cancel().await.ok();
}

//! The three tools' behaviour against a wiremock Seerr. The confirmation is injected (`ask`), so every
//! path that must file nothing is checked by counting the POSTs Seerr received.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use seerr_mcp::config::{Identity, Language, SeerrUserId};
use seerr_mcp::metrics::Metrics;
use seerr_mcp::seerr::{MediaType, Seerr};
use seerr_mcp::tools::{self, Answer, RequestArgs, Seasons};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DECLINED: &str = "Pas demandé. Redis-moi si tu changes d'avis.";

fn alice() -> Identity {
    Identity {
        profile: "alice".into(),
        token_sha256: [0; 32],
        seerr_user_id: SeerrUserId::new(NonZeroU32::new(7).unwrap()),
        jellyfin_user_id: "bb11".into(),
        matrix_room_id: None,
        language: Language::parse("fr").unwrap(),
        second_language: Some(Language::parse("en").unwrap()),
    }
}

fn bob() -> Identity {
    Identity {
        profile: "bob".into(),
        token_sha256: [0; 32],
        seerr_user_id: SeerrUserId::new(NonZeroU32::new(1).unwrap()),
        jellyfin_user_id: "aa".into(),
        matrix_room_id: None,
        language: Language::parse("en").unwrap(),
        second_language: Some(Language::parse("fr").unwrap()),
    }
}

fn counter(metrics: &Metrics, name: &str, kind: &str) -> String {
    let prefix = format!("{name}{{kind=\"{kind}\"}} ");
    metrics
        .render()
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
        .unwrap_or_else(|| panic!("no series {name}{{{kind}}}"))
}

async fn seerr_with(mocks: Vec<Mock>) -> (MockServer, Seerr) {
    let server = MockServer::start().await;
    for m in mocks {
        m.mount(&server).await;
    }
    let seerr = Seerr::new(&server.uri(), "admin-key", Duration::from_secs(5));
    (server, seerr)
}

fn get(p: &str, body: Value) -> Mock {
    Mock::given(method("GET"))
        .and(path(p.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
}

fn post_request(status: u16, body: Value) -> Mock {
    Mock::given(method("POST"))
        .and(path("/api/v1/request"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
}

async fn posts(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .count()
}

fn bluey(seasons_media: Value) -> Value {
    json!({
        "id": 82728, "name": "Bluey", "firstAirDate": "2018-10-01", "overview": "SECRET OVERVIEW",
        "seasons": [{"seasonNumber": 0}, {"seasonNumber": 1}, {"seasonNumber": 2}, {"seasonNumber": 3}],
        "mediaInfo": seasons_media
    })
}

fn paddington(media_info: Value) -> Value {
    json!({"id": 1022789, "title": "Paddington au Pérou", "releaseDate": "2024-11-08",
           "overview": "SECRET OVERVIEW", "mediaInfo": media_info})
}

fn requested_by_alice(status: u8) -> Value {
    json!({"status": status, "seasons": [], "requests": [
        {"createdAt": "2026-10-01T09:00:00.000Z", "requestedBy": {"displayName": "Alice"}}
    ]})
}

fn film(tmdb_id: u32) -> RequestArgs {
    RequestArgs {
        tmdb_id,
        media_type: MediaType::Movie,
        seasons: None,
        language: None,
    }
}

fn series(seasons: Seasons) -> RequestArgs {
    RequestArgs {
        tmdb_id: 82728,
        media_type: MediaType::Tv,
        seasons: Some(seasons),
        language: None,
    }
}

/// An `ask` that answers `answer` and records the question it was shown.
fn answering(
    answer: Answer,
    asked: &std::sync::Mutex<Vec<String>>,
) -> impl FnOnce(String) -> std::future::Ready<Answer> + '_ {
    move |q| {
        asked.lock().unwrap().push(q);
        std::future::ready(answer)
    }
}

async fn run(
    seerr: &Seerr,
    metrics: &Metrics,
    args: RequestArgs,
    answer: Answer,
    cancelled: bool,
) -> (Result<String, String>, Vec<String>) {
    run_as(&alice(), seerr, metrics, args, answer, cancelled).await
}

async fn run_as(
    who: &Identity,
    seerr: &Seerr,
    metrics: &Metrics,
    args: RequestArgs,
    answer: Answer,
    cancelled: bool,
) -> (Result<String, String>, Vec<String>) {
    let asked = std::sync::Mutex::new(Vec::new());
    let out = tools::request_media(
        seerr,
        metrics,
        who,
        args,
        true,
        answering(answer, &asked),
        || cancelled,
    )
    .await;
    (out, asked.into_inner().unwrap())
}

// ---- request_media ----

#[tokio::test]
async fn files_nothing_when_the_client_cannot_confirm() {
    let (server, seerr) =
        seerr_with(vec![get("/api/v1/movie/1022789", paddington(Value::Null))]).await;
    let metrics = Metrics::new();
    let calls = AtomicUsize::new(0);
    let out = tools::request_media(
        &seerr,
        &metrics,
        &alice(),
        film(1022789),
        false,
        |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Answer::Accepted)
        },
        || false,
    )
    .await
    .unwrap();
    assert!(out.contains("confirm"), "{out}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(posts(&server).await, 0);
    assert_eq!(
        counter(
            &metrics,
            "seerr_mcp_request_failures_total",
            "elicitation_unsupported"
        ),
        "1"
    );
}

#[tokio::test]
async fn an_accepted_film_is_filed_as_the_user() {
    let (server, seerr) = seerr_with(vec![
        get("/api/v1/movie/1022789", paddington(Value::Null)),
        Mock::given(method("POST"))
            .and(path("/api/v1/request"))
            .and(header("x-api-user", "7"))
            .and(body_json(json!({"mediaType": "movie", "mediaId": 1022789})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
            .expect(1),
    ])
    .await;
    let metrics = Metrics::new();
    let (out, asked) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    assert_eq!(
        asked,
        vec!["Demander « Paddington au Pérou » (2024), film ?"]
    );
    assert!(out.unwrap().contains("Paddington au Pérou"));
    assert_eq!(posts(&server).await, 1);
}

#[tokio::test]
async fn the_confirmation_names_the_series_and_season() {
    let (server, seerr) = seerr_with(vec![
        get("/api/v1/tv/82728", bluey(Value::Null)),
        Mock::given(method("POST"))
            .and(path("/api/v1/request"))
            .and(body_json(
                json!({"mediaType": "tv", "mediaId": 82728, "seasons": [3]}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
            .expect(1),
    ])
    .await;
    let (out, asked) = run(
        &seerr,
        &Metrics::new(),
        series(Seasons::List(vec![3])),
        Answer::Accepted,
        false,
    )
    .await;
    assert_eq!(asked, vec!["Demander « Bluey » (2018), série, saison 3 ?"]);
    out.unwrap();
    drop(server);
}

#[tokio::test]
async fn all_expands_to_the_seasons_not_yet_covered() {
    // Season 1 is available, so `all` asks for 2 and 3 only, and says so.
    let media = json!({"status": 4, "requests": [], "seasons": [{"seasonNumber": 1, "status": 5}]});
    let (server, seerr) = seerr_with(vec![
        get("/api/v1/tv/82728", bluey(media)),
        Mock::given(method("POST"))
            .and(path("/api/v1/request"))
            .and(body_json(
                json!({"mediaType": "tv", "mediaId": 82728, "seasons": [2, 3]}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
            .expect(1),
    ])
    .await;
    let (out, asked) = run(
        &seerr,
        &Metrics::new(),
        series(Seasons::All(tools::All::All)),
        Answer::Accepted,
        false,
    )
    .await;
    assert!(
        asked[0].starts_with("Demander « Bluey » (2018), série, saisons 2, 3 ?"),
        "{}",
        asked[0]
    );
    assert!(!asked[0].contains("Request"), "{}", asked[0]);
    out.unwrap();
    drop(server);
}

#[tokio::test]
async fn an_unknown_season_is_refused_before_asking() {
    let (server, seerr) = seerr_with(vec![get("/api/v1/tv/82728", bluey(Value::Null))]).await;
    let (out, asked) = run(
        &seerr,
        &Metrics::new(),
        series(Seasons::List(vec![9])),
        Answer::Accepted,
        false,
    )
    .await;
    let msg = out.unwrap_err();
    assert!(msg.contains('9') && msg.contains("1, 2, 3"), "{msg}");
    assert!(asked.is_empty());
    assert_eq!(posts(&server).await, 0);
}

#[tokio::test]
async fn a_series_without_seasons_is_refused_before_asking() {
    let (server, seerr) = seerr_with(vec![get("/api/v1/tv/82728", bluey(Value::Null))]).await;
    let args = RequestArgs {
        seasons: None,
        ..series(Seasons::List(vec![]))
    };
    let (out, asked) = run(&seerr, &Metrics::new(), args, Answer::Accepted, false).await;
    assert!(out.unwrap_err().contains("1, 2, 3"));
    assert!(asked.is_empty());
    assert_eq!(posts(&server).await, 0);
}

#[tokio::test]
async fn declining_files_nothing() {
    let (server, seerr) =
        seerr_with(vec![get("/api/v1/movie/1022789", paddington(Value::Null))]).await;
    let metrics = Metrics::new();
    let (out, asked) = run(&seerr, &metrics, film(1022789), Answer::Declined, false).await;
    assert_eq!(out.unwrap(), DECLINED);
    assert_eq!(asked.len(), 1);
    assert_eq!(posts(&server).await, 0);
    assert_eq!(
        counter(&metrics, "seerr_mcp_request_skipped_total", "declined"),
        "1"
    );
}

#[tokio::test]
async fn no_answer_files_nothing() {
    let (server, seerr) =
        seerr_with(vec![get("/api/v1/movie/1022789", paddington(Value::Null))]).await;
    let metrics = Metrics::new();
    let (out, _) = run(&seerr, &metrics, film(1022789), Answer::NoAnswer, false).await;
    assert_eq!(out.unwrap(), DECLINED);
    assert_eq!(posts(&server).await, 0);
    assert_eq!(
        counter(&metrics, "seerr_mcp_request_failures_total", "timeout"),
        "1"
    );
}

// ---- the language the person wrote in ----

/// A film whose details answer in `lang` only, so a lookup in any other language finds nothing.
fn paddington_in(lang: &str, title: &str) -> Mock {
    details_in(
        "/api/v1/movie/1022789",
        lang,
        json!({"id": 1022789, "title": title, "releaseDate": "2024-11-08", "mediaInfo": null}),
    )
}

fn film_in(language: Option<&str>) -> RequestArgs {
    RequestArgs {
        language: language.map(str::to_string),
        ..film(1022789)
    }
}

// Someone on a French profile who writes in English is asked in English, about the English title.
#[tokio::test]
async fn an_english_writer_on_a_french_profile_is_asked_in_english() {
    for code in ["en", " EN ", "en-US", "EN_gb"] {
        let (server, seerr) = seerr_with(vec![paddington_in("en", "Paddington in Peru")]).await;
        let metrics = Metrics::new();
        let (out, asked) = run(
            &seerr,
            &metrics,
            film_in(Some(code)),
            Answer::Declined,
            false,
        )
        .await;
        assert_eq!(
            asked,
            vec!["Request \"Paddington in Peru\" (2024), film?"],
            "{code:?}"
        );
        assert_eq!(
            out.unwrap(),
            "Not requested. Tell me if you change your mind."
        );
        assert_eq!(posts(&server).await, 0);
        assert_eq!(
            counter(
                &metrics,
                "seerr_mcp_language_skipped_total",
                "unsupported_language"
            ),
            "0"
        );
    }
}

#[tokio::test]
async fn without_a_language_the_profile_s_is_used_and_not_counted() {
    // Blank is absent, and "fr_FR" is the profile's own language named with a region.
    for code in [None, Some(""), Some("  "), Some("fr_FR")] {
        let (_server, seerr) = seerr_with(vec![paddington_in("fr", "Paddington au Pérou")]).await;
        let metrics = Metrics::new();
        let (out, asked) = run(&seerr, &metrics, film_in(code), Answer::Declined, false).await;
        assert_eq!(
            asked,
            vec!["Demander « Paddington au Pérou » (2024), film ?"],
            "{code:?}"
        );
        assert_eq!(out.unwrap(), DECLINED);
        assert_eq!(
            counter(
                &metrics,
                "seerr_mcp_language_skipped_total",
                "unsupported_language"
            ),
            "0",
            "{code:?}"
        );
    }
}

#[tokio::test]
async fn a_language_without_a_locale_falls_back_to_the_profile_s_and_is_counted() {
    for code in ["de", "de-CH", "english", "-en"] {
        let (_server, seerr) = seerr_with(vec![paddington_in("fr", "Paddington au Pérou")]).await;
        let metrics = Metrics::new();
        let (out, asked) = run(
            &seerr,
            &metrics,
            film_in(Some(code)),
            Answer::Declined,
            false,
        )
        .await;
        assert_eq!(
            asked,
            vec!["Demander « Paddington au Pérou » (2024), film ?"],
            "{code:?}"
        );
        assert_eq!(out.unwrap(), DECLINED);
        assert_eq!(
            counter(
                &metrics,
                "seerr_mcp_language_skipped_total",
                "unsupported_language"
            ),
            "1",
            "{code:?}"
        );
    }
}

#[test]
fn language_is_optional_in_the_arguments() {
    let args: RequestArgs =
        serde_json::from_value(json!({"tmdb_id": 1022789, "type": "movie"})).unwrap();
    assert_eq!(args.language, None);
    let args: RequestArgs =
        serde_json::from_value(json!({"tmdb_id": 1022789, "type": "movie", "language": "en"}))
            .unwrap();
    assert_eq!(args.language.as_deref(), Some("en"));
}

#[tokio::test]
async fn an_accept_after_the_call_was_cancelled_files_nothing() {
    let (server, seerr) =
        seerr_with(vec![get("/api/v1/movie/1022789", paddington(Value::Null))]).await;
    let metrics = Metrics::new();
    let (out, _) = run(&seerr, &metrics, film(1022789), Answer::Accepted, true).await;
    assert_eq!(out.unwrap(), DECLINED);
    assert_eq!(posts(&server).await, 0);
    assert_eq!(
        counter(&metrics, "seerr_mcp_request_failures_total", "timeout"),
        "1"
    );
}

#[tokio::test]
async fn an_available_title_is_reported_without_asking() {
    let (server, seerr) = seerr_with(vec![get(
        "/api/v1/movie/1022789",
        paddington(requested_by_alice(5)),
    )])
    .await;
    let metrics = Metrics::new();
    let (out, asked) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    assert!(out.unwrap().contains("already available"));
    assert!(asked.is_empty());
    assert_eq!(posts(&server).await, 0);
    assert_eq!(
        counter(
            &metrics,
            "seerr_mcp_request_skipped_total",
            "already_available"
        ),
        "1"
    );
}

#[tokio::test]
async fn a_requested_title_names_who_and_when_without_asking() {
    let (server, seerr) = seerr_with(vec![get(
        "/api/v1/movie/1022789",
        paddington(requested_by_alice(2)),
    )])
    .await;
    let metrics = Metrics::new();
    let (out, asked) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    let out = out.unwrap();
    assert!(
        out.contains("already requested by Alice on 2026-10-01"),
        "{out}"
    );
    assert!(asked.is_empty());
    assert_eq!(posts(&server).await, 0);
    assert_eq!(
        counter(&metrics, "seerr_mcp_request_skipped_total", "duplicate"),
        "1"
    );
}

#[tokio::test]
async fn requested_seasons_already_covered_are_reported_without_asking() {
    let media = json!({"status": 3, "seasons": [{"seasonNumber": 3, "status": 2}], "requests": [
        {"createdAt": "2026-10-02T09:00:00.000Z", "requestedBy": {"displayName": "Bob"}}
    ]});
    let (server, seerr) = seerr_with(vec![get("/api/v1/tv/82728", bluey(media))]).await;
    let (out, asked) = run(
        &seerr,
        &Metrics::new(),
        series(Seasons::List(vec![3])),
        Answer::Accepted,
        false,
    )
    .await;
    assert!(out
        .unwrap()
        .contains("already requested by Bob on 2026-10-02"));
    assert!(asked.is_empty());
    assert_eq!(posts(&server).await, 0);
}

#[tokio::test]
async fn a_409_reads_back_who_requested_it() {
    // Someone filed between the re-read and the POST.
    let server = MockServer::start().await;
    get("/api/v1/movie/1022789", paddington(Value::Null))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    get("/api/v1/movie/1022789", paddington(requested_by_alice(2)))
        .mount(&server)
        .await;
    post_request(
        409,
        json!({"message": "Request for this media already exists."}),
    )
    .mount(&server)
    .await;
    let seerr = Seerr::new(&server.uri(), "k", Duration::from_secs(5));
    let metrics = Metrics::new();
    let (out, _) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    assert!(out
        .unwrap()
        .contains("already requested by Alice on 2026-10-01"));
    assert_eq!(
        counter(&metrics, "seerr_mcp_request_skipped_total", "duplicate"),
        "1"
    );
}

#[tokio::test]
async fn a_used_up_quota_is_said_and_counted() {
    let (_server, seerr) = seerr_with(vec![
        get("/api/v1/movie/1022789", paddington(Value::Null)),
        post_request(403, json!({"message": "Movie Quota exceeded."})),
    ])
    .await;
    let metrics = Metrics::new();
    let (out, _) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    assert!(out.unwrap().contains("quota"));
    assert_eq!(
        counter(&metrics, "seerr_mcp_request_skipped_total", "quota"),
        "1"
    );
}

#[tokio::test]
async fn seerr_faults_are_errors_and_counted_by_kind() {
    let (_server, seerr) = seerr_with(vec![
        get("/api/v1/movie/1022789", paddington(Value::Null)),
        post_request(
            403,
            json!({"message": "You do not have permission to make movie requests."}),
        ),
    ])
    .await;
    let metrics = Metrics::new();
    let (out, _) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    assert!(out.is_err());
    assert_eq!(
        counter(
            &metrics,
            "seerr_mcp_seerr_failures_total",
            "permission_denied"
        ),
        "1"
    );

    let (_server, seerr) = seerr_with(vec![
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(503))
    ])
    .await;
    let (out, _) = run(&seerr, &metrics, film(1022789), Answer::Accepted, false).await;
    assert!(out.is_err());
    assert_eq!(
        counter(&metrics, "seerr_mcp_seerr_failures_total", "http_5xx"),
        "1"
    );

    let dead = Seerr::new("http://127.0.0.1:9", "k", Duration::from_secs(2));
    let (out, _) = run(&dead, &metrics, film(1022789), Answer::Accepted, false).await;
    assert!(out.is_err());
    assert_eq!(
        counter(&metrics, "seerr_mcp_seerr_failures_total", "unreachable"),
        "1"
    );
}

// ---- search_media ----

#[tokio::test]
async fn search_gives_the_top_three_without_overviews() {
    let (_server, seerr) = seerr_with(vec![
        get(
            "/api/v1/search",
            json!({"results": [
                {"mediaType": "movie", "id": 1022789},
                {"mediaType": "person", "id": 5},
                {"mediaType": "tv", "id": 82728},
                {"mediaType": "movie", "id": 2},
                {"mediaType": "movie", "id": 3},
            ]}),
        ),
        get("/api/v1/movie/1022789", paddington(requested_by_alice(2))),
        get(
            "/api/v1/tv/82728",
            bluey(json!({"status": 5, "requests": [], "seasons": []})),
        ),
        get(
            "/api/v1/movie/2",
            json!({"title": "Pad\u{1b}[31mdington\u{7}", "releaseDate": ""}),
        ),
        get("/api/v1/movie/3", json!({"title": "Fourth"})),
    ])
    .await;
    let out = tools::search_media(&seerr, &Metrics::new(), &alice(), "paddington")
        .await
        .unwrap();
    assert!(!out.contains("SECRET OVERVIEW"), "{out}");
    assert!(!out.contains("Fourth"), "top 3 only: {out}");
    assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
    assert!(out.contains("Pad[31mdington"), "{out}");
    assert!(
        out.contains(
            "film « Paddington au Pérou » (2024), tmdb 1022789: requested by Alice on 2026-10-01"
        ),
        "{out}"
    );
    assert!(
        out.contains("series « Bluey » (2018), tmdb 82728, seasons 1, 2, 3: available"),
        "{out}"
    );
    assert!(out.contains("tmdb 2: not requested yet"), "{out}");
}

fn details_in(p: &str, lang: &str, body: Value) -> Mock {
    Mock::given(method("GET"))
        .and(path(p.to_string()))
        .and(query_param("language", lang))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
}

// TMDB finds a film by any of its titles, but shows it in the profile's language only, so a
// French profile asking for "The Dinner Game" saw "Le Dîner de cons" below "The After Dinner Game".
#[tokio::test]
async fn a_hit_shows_its_french_and_english_titles_on_one_line() {
    let after = json!({"title": "The After Dinner Game", "originalTitle": "The After Dinner Game", "releaseDate": "1975-01-01"});
    let (_server, seerr) = seerr_with(vec![
        get(
            "/api/v1/search",
            json!({"results": [{"mediaType": "movie", "id": 743254}, {"mediaType": "movie", "id": 9421}]}),
        ),
        get("/api/v1/movie/743254", after),
        details_in(
            "/api/v1/movie/9421",
            "fr",
            json!({"title": "Le Dîner de cons", "originalTitle": "Le Dîner de cons", "releaseDate": "1998-04-15"}),
        ),
        details_in(
            "/api/v1/movie/9421",
            "en",
            json!({"title": "The Dinner Game", "originalTitle": "Le Dîner de cons", "releaseDate": "1998-04-15"}),
        ),
    ])
    .await;
    let out = tools::search_media(&seerr, &Metrics::new(), &alice(), "The Dinner Game")
        .await
        .unwrap();
    assert!(
        out.contains(
            "film « Le Dîner de cons » / « The Dinner Game » (1998), tmdb 9421: not requested yet"
        ),
        "{out}"
    );
    // One title in both languages is shown once.
    assert!(
        out.contains("film « The After Dinner Game » (1975), tmdb 743254"),
        "{out}"
    );
}

#[tokio::test]
async fn the_person_s_language_comes_first_and_a_third_original_is_named() {
    let (_server, seerr) = seerr_with(vec![
        get(
            "/api/v1/search",
            json!({"results": [{"mediaType": "movie", "id": 45138}]}),
        ),
        details_in(
            "/api/v1/movie/45138",
            "fr",
            json!({"title": "Les Intouchables", "originalTitle": "Gli intoccabili", "releaseDate": "1969-01-01"}),
        ),
        details_in(
            "/api/v1/movie/45138",
            "en",
            json!({"title": "Machine Gun McCain", "originalTitle": "Gli intoccabili", "releaseDate": "1969-01-01"}),
        ),
    ])
    .await;
    let out = tools::search_media(&seerr, &Metrics::new(), &bob(), "x")
        .await
        .unwrap();
    assert!(
        out.contains("film « Machine Gun McCain » / « Les Intouchables » (original « Gli intoccabili », 1969), tmdb 45138"),
        "{out}"
    );
}

#[tokio::test]
async fn a_series_carries_both_names_too() {
    let (_server, seerr) = seerr_with(vec![
        get("/api/v1/search", json!({"results": [{"mediaType": "tv", "id": 1}]})),
        details_in(
            "/api/v1/tv/1",
            "fr",
            json!({"name": "Le Bureau des légendes", "originalName": "Le Bureau des légendes", "firstAirDate": "2015-04-27"}),
        ),
        details_in(
            "/api/v1/tv/1",
            "en",
            json!({"name": "The Bureau", "originalName": "Le Bureau des légendes", "firstAirDate": "2015-04-27"}),
        ),
    ])
    .await;
    let out = tools::search_media(&seerr, &Metrics::new(), &alice(), "x")
        .await
        .unwrap();
    assert!(
        out.contains("series « Le Bureau des légendes » / « The Bureau » (2015), tmdb 1"),
        "{out}"
    );
}

// Six details calls (three hits, two languages) must not run one after another.
#[tokio::test]
async fn details_calls_run_concurrently() {
    let server = MockServer::start().await;
    get(
        "/api/v1/search",
        json!({"results": [{"mediaType": "movie", "id": 1}, {"mediaType": "movie", "id": 2}, {"mediaType": "movie", "id": 3}]}),
    )
    .mount(&server)
    .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/api/v1/movie/\d+$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"title": "T"}))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let seerr = Seerr::new(&server.uri(), "admin-key", Duration::from_secs(5));
    let start = std::time::Instant::now();
    tools::search_media(&seerr, &Metrics::new(), &alice(), "x")
        .await
        .unwrap();
    let details = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/api/v1/movie/"))
        .count();
    assert_eq!(details, 6);
    // All six at once take one 300 ms delay; awaiting either the languages or the hits in turn takes
    // two or three, so the bound sits below 600 ms.
    assert!(
        start.elapsed() < Duration::from_millis(550),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn titles_are_capped() {
    let long = "x".repeat(1000);
    let (_server, seerr) = seerr_with(vec![
        get(
            "/api/v1/search",
            json!({"results": [{"mediaType": "movie", "id": 2}]}),
        ),
        get("/api/v1/movie/2", json!({"title": long})),
    ])
    .await;
    let out = tools::search_media(&seerr, &Metrics::new(), &alice(), "x")
        .await
        .unwrap();
    assert!(out.len() < 400, "{}", out.len());
}

#[tokio::test]
async fn search_with_no_hits_says_so() {
    let (_server, seerr) = seerr_with(vec![get("/api/v1/search", json!({"results": []}))]).await;
    let out = tools::search_media(&seerr, &Metrics::new(), &alice(), "zzz")
        .await
        .unwrap();
    assert!(out.contains("Nothing found"), "{out}");
    // Search ignores language, so another title or one distinctive word is the model's next move.
    // The server cannot count retries, so the bound and the way out are in the text itself. The
    // retry's own miss gets this same text, so it must say, per title, that the retry is the last.
    assert!(
        out.contains(
            "If this was your first search for it, try one more, with the English or original \
             title or one distinctive word of it."
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "If it was already your second, stop searching: tell the person nothing matched and \
             ask what else they remember of it."
        ),
        "{out}"
    );
    assert!(!out.contains("web"), "{out}");
}

// A real miss: Seerr finds nothing once a year follows the title, even the right one.
#[tokio::test]
async fn a_trailing_year_is_dropped_when_it_finds_nothing() {
    for query in ["Soldat Collins 2024", "Soldat Collins (2024)"] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search"))
            .and(query_param("query", query))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search"))
            .and(query_param("query", "Soldat Collins"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"results": [{"mediaType": "movie", "id": 981063}]})),
            )
            .mount(&server)
            .await;
        get("/api/v1/movie/981063", json!({"title": "Soldat Collins", "originalTitle": "Before Dawn", "releaseDate": "2024-06-12"}))
            .mount(&server)
            .await;
        let seerr = Seerr::new(&server.uri(), "admin-key", Duration::from_secs(5));
        let out = tools::search_media(&seerr, &Metrics::new(), &alice(), query)
            .await
            .unwrap();
        assert!(out.contains("tmdb 981063"), "{query}: {out}");
    }
}

// The retry is for a year only: a query without one is searched once.
#[tokio::test]
async fn a_query_without_a_year_is_searched_once() {
    let (server, seerr) = seerr_with(vec![get("/api/v1/search", json!({"results": []}))]).await;
    for query in ["Arnaque crime et botanique", "Blade Runner 2049", "1917"] {
        tools::search_media(&seerr, &Metrics::new(), &alice(), query)
            .await
            .unwrap();
    }
    let searches = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/search")
        .count();
    // "Blade Runner 2049" ends in a year-shaped number, so it is retried as "Blade Runner": 4 searches.
    assert_eq!(searches, 4);
}

// ---- my_requests ----

#[tokio::test]
async fn my_requests_gives_three_statuses_and_titles() {
    let row = |tmdb: u32, kind: &str, req: u8, media: u8| {
        json!({"status": req, "createdAt": "2026-10-12T10:00:00.000Z",
               "media": {"tmdbId": tmdb, "mediaType": kind, "status": media}, "seasons": []})
    };
    let (_server, seerr) = seerr_with(vec![
        get(
            "/api/v1/user/7/requests",
            json!({"results": [
                row(1022789, "movie", 2, 5),
                row(82728, "tv", 2, 3),
                row(2, "movie", 1, 2),
                row(3, "movie", 3, 2),
            ]}),
        ),
        get("/api/v1/movie/1022789", paddington(Value::Null)),
        get("/api/v1/tv/82728", bluey(Value::Null)),
        get("/api/v1/movie/2", json!({"title": "Two"})),
        get("/api/v1/movie/3", json!({"title": "Three"})),
    ])
    .await;
    let out = tools::my_requests(&seerr, &Metrics::new(), &alice())
        .await
        .unwrap();
    assert!(
        out.contains("« Paddington au Pérou » (2024), 2026-10-12: ready on Jellyfin"),
        "{out}"
    );
    assert!(
        out.contains("« Bluey » (2018), 2026-10-12: downloading"),
        "{out}"
    );
    assert!(out.contains("« Two », 2026-10-12: requested"), "{out}");
    assert!(out.contains("« Three », 2026-10-12: declined"), "{out}");
}

#[tokio::test]
async fn my_requests_with_none_says_so() {
    let (_server, seerr) =
        seerr_with(vec![get("/api/v1/user/7/requests", json!({"results": []}))]).await;
    let out = tools::my_requests(&seerr, &Metrics::new(), &alice())
        .await
        .unwrap();
    assert!(out.contains("No requests yet"), "{out}");
}

#[tokio::test]
async fn an_unknown_answer_from_seerr_counts_as_unexpected() {
    let (_server, seerr) = seerr_with(vec![
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404))
    ])
    .await;
    let metrics = Metrics::new();
    let (out, _) = run(&seerr, &metrics, film(1), Answer::Accepted, false).await;
    assert!(out.is_err());
    let (_server, seerr) = seerr_with(vec![get(
        "/api/v1/user/7/requests",
        json!({"results": "no"}),
    )])
    .await;
    assert!(tools::my_requests(&seerr, &metrics, &alice())
        .await
        .is_err());
    assert_eq!(
        counter(&metrics, "seerr_mcp_seerr_failures_total", "unexpected"),
        "2"
    );
}

#[tokio::test]
async fn an_english_speaker_is_asked_in_english_only() {
    let (server, seerr) = seerr_with(vec![get("/api/v1/tv/82728", bluey(Value::Null))]).await;
    let metrics = Metrics::new();
    let (out, asked) = run_as(
        &bob(),
        &seerr,
        &metrics,
        series(Seasons::List(vec![3])),
        Answer::Declined,
        false,
    )
    .await;
    assert_eq!(asked, vec!["Request \"Bluey\" (2018), series, season 3?"]);
    assert_eq!(
        out.unwrap(),
        "Not requested. Tell me if you change your mind."
    );
    drop(server);
}

#[tokio::test]
async fn an_english_speaker_is_asked_about_a_film_in_english() {
    let (server, seerr) =
        seerr_with(vec![get("/api/v1/movie/1022789", paddington(Value::Null))]).await;
    let (_, asked) = run_as(
        &bob(),
        &seerr,
        &Metrics::new(),
        film(1022789),
        Answer::Declined,
        false,
    )
    .await;
    assert_eq!(asked, vec!["Request \"Paddington au Pérou\" (2024), film?"]);
    drop(server);
}

#[tokio::test]
async fn an_english_speaker_is_asked_about_several_seasons_in_english() {
    let (server, seerr) = seerr_with(vec![get("/api/v1/tv/82728", bluey(Value::Null))]).await;
    let (_, asked) = run_as(
        &bob(),
        &seerr,
        &Metrics::new(),
        series(Seasons::List(vec![2, 3])),
        Answer::Declined,
        false,
    )
    .await;
    assert_eq!(
        asked,
        vec!["Request \"Bluey\" (2018), series, seasons 2, 3?"]
    );
    drop(server);
}

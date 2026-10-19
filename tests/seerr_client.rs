//! The Seerr client against a wiremock Seerr: paths, the acting-user header, and how each refusal is
//! classified. Shapes follow seerr-team/seerr at 1313800 (routes/request.ts, routes/user/index.ts).

use std::num::NonZeroU32;
use std::time::Duration;

use seerr_mcp::config::SeerrUserId;
use seerr_mcp::seerr::{MediaType, Seerr, SeerrError};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user(id: u32) -> SeerrUserId {
    SeerrUserId::new(NonZeroU32::new(id).unwrap())
}

fn client(server: &MockServer) -> Seerr {
    Seerr::new(&server.uri(), "admin-key", Duration::from_secs(5))
}

fn movie_details() -> serde_json::Value {
    json!({
        "id": 1022789, "title": "Paddington au Pérou", "releaseDate": "2024-11-08",
        "overview": "SECRET OVERVIEW",
        "mediaInfo": {
            "status": 2,
            "seasons": [],
            "requests": [{"createdAt": "2026-10-01T09:00:00.000Z", "requestedBy": {"displayName": "Alice"}}]
        }
    })
}

fn tv_details() -> serde_json::Value {
    json!({
        "id": 82728, "name": "Bluey", "firstAirDate": "2018-10-01",
        "seasons": [{"seasonNumber": 0}, {"seasonNumber": 1}, {"seasonNumber": 2}, {"seasonNumber": 3}]
    })
}

#[tokio::test]
async fn every_call_carries_the_acting_user_and_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/movie/1022789"))
        .respond_with(ResponseTemplate::new(200).set_body_json(movie_details()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/request"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/7/requests"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;

    let seerr = client(&server);
    seerr
        .details(user(7), MediaType::Movie, 1022789, "fr")
        .await
        .unwrap();
    seerr
        .request(user(7), MediaType::Movie, 1022789, None)
        .await
        .unwrap();
    seerr.user_requests(user(7), 10).await.unwrap();
    seerr.search(user(7), "paddington", "fr").await.unwrap();

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 4);
    for r in received {
        assert_eq!(
            r.headers.get("x-api-user").unwrap(),
            "7",
            "{} {}",
            r.method,
            r.url
        );
        assert_eq!(r.headers.get("x-api-key").unwrap(), "admin-key");
    }
}

#[tokio::test]
async fn details_read_title_year_seasons_and_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/tv/82728"))
        .and(query_param("language", "en"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tv_details()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/movie/1022789"))
        .respond_with(ResponseTemplate::new(200).set_body_json(movie_details()))
        .mount(&server)
        .await;

    let seerr = client(&server);
    let tv = seerr
        .details(user(1), MediaType::Tv, 82728, "en")
        .await
        .unwrap();
    assert_eq!(tv.title, "Bluey");
    assert_eq!(tv.year.as_deref(), Some("2018"));
    assert_eq!(
        tv.seasons,
        vec![1, 2, 3],
        "season 0 (specials) is not requestable"
    );
    assert!(tv.media.is_none());

    let movie = seerr
        .details(user(1), MediaType::Movie, 1022789, "fr")
        .await
        .unwrap();
    let media = movie.media.unwrap();
    assert_eq!(media.status, 2);
    assert_eq!(media.requests[0].requested_by, "Alice");
    assert_eq!(media.requests[0].created_at, "2026-10-01T09:00:00.000Z");
}

#[tokio::test]
async fn search_keeps_films_and_series_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(query_param("query", "bluey & co"))
        .and(query_param("language", "fr"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"mediaType": "person", "id": 5},
            {"mediaType": "tv", "id": 82728},
            {"mediaType": "movie", "id": 1022789},
            {"mediaType": "collection", "id": 9},
        ]})))
        .mount(&server)
        .await;

    let hits = client(&server)
        .search(user(1), "bluey & co", "fr")
        .await
        .unwrap();
    assert_eq!(
        hits,
        vec![(MediaType::Tv, 82728), (MediaType::Movie, 1022789)]
    );
}

/// Seerr refuses a query value holding a reserved character ("must be url encoded"), and `+` for a
/// space is one: every search of more than one word failed with a 400 (live, 2026-10-17).
#[tokio::test]
async fn search_sends_the_query_without_reserved_characters() {
    struct StrictlyEncoded;
    impl wiremock::Match for StrictlyEncoded {
        fn matches(&self, request: &wiremock::Request) -> bool {
            request.url.query().unwrap_or("").split('&').all(|pair| {
                pair.split_once('=').is_some_and(|(_, v)| {
                    v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b))
                })
            })
        }
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/search"))
        .and(StrictlyEncoded)
        .and(query_param("query", "ocean's eleven (2001) + co*"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"results": [{"mediaType": "movie", "id": 161}]})),
        )
        .mount(&server)
        .await;

    let hits = client(&server)
        .search(user(1), "ocean's eleven (2001) + co*", "en")
        .await
        .unwrap();
    assert_eq!(hits, vec![(MediaType::Movie, 161)]);
}

#[tokio::test]
async fn request_posts_the_seasons_for_a_series() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/request"))
        .and(header("x-api-user", "7"))
        .and(body_json(
            json!({"mediaType": "tv", "mediaId": 82728, "seasons": [3]}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .request(user(7), MediaType::Tv, 82728, Some(&[3]))
        .await
        .unwrap();
}

#[tokio::test]
async fn request_posts_no_seasons_for_a_film() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/request"))
        .and(body_json(json!({"mediaType": "movie", "mediaId": 1022789})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .request(user(7), MediaType::Movie, 1022789, None)
        .await
        .unwrap();
}

async fn post_answering(status: u16, message: &str) -> SeerrError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/request"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({"message": message})))
        .mount(&server)
        .await;
    client(&server)
        .request(user(7), MediaType::Movie, 1, None)
        .await
        .unwrap_err()
}

#[tokio::test]
async fn refusals_are_classified() {
    assert!(matches!(
        post_answering(409, "Request for this media already exists.").await,
        SeerrError::Duplicate
    ));
    assert!(matches!(
        post_answering(403, "Movie Quota exceeded.").await,
        SeerrError::Quota
    ));
    assert!(matches!(
        post_answering(403, "Series Quota exceeded.").await,
        SeerrError::Quota
    ));
    assert!(matches!(
        post_answering(403, "You do not have permission to make movie requests.").await,
        SeerrError::PermissionDenied
    ));
    assert!(matches!(
        post_answering(202, "No seasons available to request").await,
        SeerrError::NothingToRequest
    ));
    assert!(matches!(
        post_answering(500, "boom").await,
        SeerrError::Http5xx(500)
    ));
    assert!(matches!(
        post_answering(404, "nope").await,
        SeerrError::Unexpected(404)
    ));
}

#[tokio::test]
async fn a_dead_seerr_is_unreachable() {
    let seerr = Seerr::new("http://127.0.0.1:9", "k", Duration::from_secs(2));
    let e = seerr
        .details(user(1), MediaType::Movie, 1, "en")
        .await
        .unwrap_err();
    assert!(matches!(e, SeerrError::Unreachable), "{e:?}");
}

#[tokio::test]
async fn a_slow_seerr_times_out_as_unreachable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let seerr = Seerr::new(&server.uri(), "k", Duration::from_millis(200));
    let e = seerr
        .details(user(1), MediaType::Movie, 1, "en")
        .await
        .unwrap_err();
    assert!(matches!(e, SeerrError::Unreachable), "{e:?}");
}

#[tokio::test]
async fn user_requests_reads_the_newest_first_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user/7/requests"))
        .and(query_param("take", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "pageInfo": {"pages": 1},
            "results": [{
                "id": 42, "status": 2, "createdAt": "2026-10-12T10:00:00.000Z",
                "media": {"tmdbId": 82728, "mediaType": "tv", "status": 3},
                "seasons": [{"seasonNumber": 3}]
            }]
        })))
        .mount(&server)
        .await;
    let rows = client(&server).user_requests(user(7), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].media_type, MediaType::Tv);
    assert_eq!(rows[0].tmdb_id, 82728);
    assert_eq!(rows[0].request_status, 2);
    assert_eq!(rows[0].media_status, 3);
    assert_eq!(rows[0].seasons, vec![3]);
    assert_eq!(rows[0].created_at, "2026-10-12T10:00:00.000Z");
}

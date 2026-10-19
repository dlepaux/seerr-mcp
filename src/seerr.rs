//! The Seerr client. Every call goes through `Seerr::call`, which takes a `SeerrUserId` by type: the
//! admin key without `X-API-User` acts as the admin, so no path may build a request without one.

use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::SeerrUserId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Movie,
    Tv,
}

impl MediaType {
    fn path(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Tv => "tv",
        }
    }
}

/// How a Seerr call failed. The first three, and `Unexpected`, are faults (counted in
/// `seerr_failures_total`); the rest are answers about the request the user should hear.
#[derive(Debug)]
pub enum SeerrError {
    /// Connection refused, DNS, TLS or the client timeout.
    Unreachable,
    Http5xx(u16),
    PermissionDenied,
    /// A status or a body this client does not know. 0 when the status was fine but the body was not.
    Unexpected(u16),
    Quota,
    Duplicate,
    /// 202 on a series request: every season is already requested or available.
    NothingToRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingRequest {
    pub requested_by: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    /// Seerr's MediaStatus: 1 unknown, 2 pending, 3 processing, 4 partially available, 5 available,
    /// 6 blocklisted, 7 deleted.
    pub status: u8,
    pub requests: Vec<ExistingRequest>,
    /// (season number, MediaStatus) for each season Seerr tracks; empty for a film.
    pub seasons: Vec<(u32, u8)>,
}

/// What the tools need of a title. The overview is never read, so it cannot reach the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    /// In the language the call asked for.
    pub title: String,
    /// In the language it was made in; empty when Seerr gives none.
    pub original_title: String,
    pub year: Option<String>,
    /// Requestable season numbers (specials, season 0, excluded); empty for a film.
    pub seasons: Vec<u32>,
    pub media: Option<MediaInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRow {
    pub media_type: MediaType,
    pub tmdb_id: u32,
    /// Seerr's MediaRequestStatus: 1 pending, 2 approved, 3 declined, 4 failed, 5 completed.
    pub request_status: u8,
    pub media_status: u8,
    pub seasons: Vec<u32>,
    pub created_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetails {
    title: Option<String>,
    name: Option<String>,
    original_title: Option<String>,
    original_name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    #[serde(default)]
    seasons: Vec<RawSeason>,
    media_info: Option<RawMediaInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeason {
    season_number: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMediaInfo {
    status: u8,
    #[serde(default)]
    requests: Vec<RawExistingRequest>,
    #[serde(default)]
    seasons: Vec<RawSeasonStatus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeasonStatus {
    season_number: u32,
    status: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawExistingRequest {
    created_at: String,
    requested_by: RawUser,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUser {
    display_name: String,
}

#[derive(Deserialize)]
struct RawPage<T> {
    results: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHit {
    media_type: String,
    id: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequestRow {
    status: u8,
    created_at: String,
    media: RawRequestMedia,
    #[serde(default)]
    seasons: Vec<RawSeason>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequestMedia {
    tmdb_id: u32,
    media_type: MediaType,
    status: u8,
}

#[derive(Deserialize, Default)]
struct RawMessage {
    #[serde(default)]
    message: String,
}

fn year(date: Option<String>) -> Option<String> {
    date.filter(|d| d.len() >= 4).map(|d| d[..4].to_string())
}

fn season_numbers(seasons: Vec<RawSeason>) -> Vec<u32> {
    seasons
        .into_iter()
        .map(|s| s.season_number)
        .filter(|n| *n > 0)
        .collect()
}

/// Percent-encodes every byte except RFC 3986's unreserved set, the only form Seerr accepts.
fn strict_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub struct Seerr {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

impl Seerr {
    /// `timeout` bounds each whole call. `main` uses 190 s, so a proxy that wakes a sleeping Seerr
    /// within a 180 s read timeout still gets an answer.
    ///
    /// # Panics
    /// If the TLS backend cannot initialise, which is a build defect, not a runtime condition.
    #[must_use]
    pub fn new(base: &str, api_key: &str, timeout: Duration) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("the rustls client builds"),
            base: base.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Url {
        let mut url = Url::parse(&format!("{}/api/v1/{path}", self.base))
            .expect("SEERR_URL plus a fixed path is a URL");
        if !query.is_empty() {
            // Not query_pairs_mut: its form encoding writes a space as `+`, and Seerr refuses any
            // reserved character in a value ("must be url encoded"), so every multi-word search 400'd.
            let pairs: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", strict_encode(k), strict_encode(v)))
                .collect();
            url.set_query(Some(&pairs.join("&")));
        }
        url
    }

    /// The only function that sends. 2xx answers come back with their status; everything else is
    /// classified here so every route reports refusals the same way.
    async fn call(
        &self,
        user: SeerrUserId,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), SeerrError> {
        let mut req = self
            .http
            .request(method, url)
            .header("X-Api-Key", &self.api_key)
            .header("X-API-User", user.get().to_string());
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await.map_err(|_| SeerrError::Unreachable)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|_| SeerrError::Unreachable)?;
        if status.is_success() {
            let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Ok((status, value));
        }
        let message = serde_json::from_slice::<RawMessage>(&bytes)
            .unwrap_or_default()
            .message;
        Err(match status.as_u16() {
            409 => SeerrError::Duplicate,
            403 if message.contains("Quota exceeded") => SeerrError::Quota,
            403 => SeerrError::PermissionDenied,
            code @ 500..=599 => SeerrError::Http5xx(code),
            code => SeerrError::Unexpected(code),
        })
    }

    async fn get<T: DeserializeOwned>(&self, user: SeerrUserId, url: Url) -> Result<T, SeerrError> {
        let (_, value) = self.call(user, Method::GET, url, None).await?;
        serde_json::from_value(value).map_err(|_| SeerrError::Unexpected(0))
    }

    /// # Errors
    /// See `SeerrError`.
    pub async fn details(
        &self,
        user: SeerrUserId,
        media_type: MediaType,
        tmdb_id: u32,
        language: &str,
    ) -> Result<Details, SeerrError> {
        let url = self.url(
            &format!("{}/{tmdb_id}", media_type.path()),
            &[("language", language)],
        );
        let raw: RawDetails = self.get(user, url).await?;
        Ok(Details {
            title: raw.title.or(raw.name).unwrap_or_default(),
            original_title: raw.original_title.or(raw.original_name).unwrap_or_default(),
            year: year(raw.release_date.or(raw.first_air_date)),
            seasons: season_numbers(raw.seasons),
            media: raw.media_info.map(|m| MediaInfo {
                status: m.status,
                requests: m
                    .requests
                    .into_iter()
                    .map(|r| ExistingRequest {
                        requested_by: r.requested_by.display_name,
                        created_at: r.created_at,
                    })
                    .collect(),
                seasons: m
                    .seasons
                    .into_iter()
                    .map(|s| (s.season_number, s.status))
                    .collect(),
            }),
        })
    }

    /// Films and series only, in Seerr's order; people and collections are dropped.
    ///
    /// # Errors
    /// See `SeerrError`.
    pub async fn search(
        &self,
        user: SeerrUserId,
        query: &str,
        language: &str,
    ) -> Result<Vec<(MediaType, u32)>, SeerrError> {
        let url = self.url(
            "search",
            &[("query", query), ("page", "1"), ("language", language)],
        );
        let page: RawPage<RawHit> = self.get(user, url).await?;
        Ok(page
            .results
            .into_iter()
            .filter_map(|h| match h.media_type.as_str() {
                "movie" => Some((MediaType::Movie, h.id)),
                "tv" => Some((MediaType::Tv, h.id)),
                _ => None,
            })
            .collect())
    }

    /// Files a request as `user`. `seasons` is ignored for a film.
    ///
    /// # Errors
    /// See `SeerrError`; `Duplicate`, `Quota` and `NothingToRequest` are answers, not faults.
    pub async fn request(
        &self,
        user: SeerrUserId,
        media_type: MediaType,
        tmdb_id: u32,
        seasons: Option<&[u32]>,
    ) -> Result<(), SeerrError> {
        let mut body = serde_json::json!({"mediaType": media_type, "mediaId": tmdb_id});
        if let (MediaType::Tv, Some(seasons)) = (media_type, seasons) {
            body["seasons"] = serde_json::json!(seasons);
        }
        let url = self.url("request", &[]);
        let (status, _) = self.call(user, Method::POST, url, Some(&body)).await?;
        if status == StatusCode::ACCEPTED {
            return Err(SeerrError::NothingToRequest);
        }
        Ok(())
    }

    /// The user's own requests, newest first (Seerr's default sort).
    ///
    /// # Errors
    /// See `SeerrError`.
    pub async fn user_requests(
        &self,
        user: SeerrUserId,
        take: u32,
    ) -> Result<Vec<RequestRow>, SeerrError> {
        let take = take.to_string();
        let url = self.url(&format!("user/{}/requests", user.get()), &[("take", &take)]);
        let page: RawPage<RawRequestRow> = self.get(user, url).await?;
        Ok(page
            .results
            .into_iter()
            .map(|r| RequestRow {
                media_type: r.media.media_type,
                tmdb_id: r.media.tmdb_id,
                request_status: r.status,
                media_status: r.media.status,
                seasons: season_numbers(r.seasons),
                created_at: r.created_at,
            })
            .collect())
    }
}

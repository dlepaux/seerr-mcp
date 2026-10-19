//! What the three tools do, as plain async functions over a `Seerr`, the counters and the caller's
//! identity. The rmcp layer (`server.rs`) only extracts the identity and the elicitation; the
//! confirmation is passed in as `ask`, so every path that must file nothing is testable without a client.
//!
//! `Ok` is text for the model; `Err` is text too, returned as a tool error.

use std::future::Future;

use rmcp::schemars;
use serde::Deserialize;

use crate::config::{Identity, Language};
use crate::metrics::{Metrics, REQUEST_FAILURES, REQUEST_SKIPPED, SEERR_FAILURES};
use crate::seerr::{Details, MediaInfo, MediaType, Seerr, SeerrError};

/// TMDB titles and Seerr display names are untrusted input to the model.
const MAX_TITLE: usize = 120;
const MAX_NAME: usize = 64;
const SEARCH_RESULTS: usize = 3;
const MY_REQUESTS: u32 = 10;

/// What a declined or unanswered confirmation answers, in the person's language.
#[must_use]
pub fn not_requested(lang: Language) -> &'static str {
    match lang {
        Language::Fr => "Pas demandé. Redis-moi si tu changes d'avis.",
        Language::En => "Not requested. Tell me if you change your mind.",
    }
}

/// The user's answer to the confirmation. A dismissed form is `Declined`; a timeout or a broken
/// exchange is `NoAnswer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accepted,
    Declined,
    NoAnswer,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum All {
    All,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Seasons {
    /// Every season not yet available or requested.
    All(All),
    /// Season numbers, as `search_media` lists them.
    List(Vec<u32>),
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestArgs {
    /// The TMDB id `search_media` gave.
    pub tmdb_id: u32,
    /// `movie` or `tv`, as `search_media` gave.
    #[serde(rename = "type")]
    pub media_type: MediaType,
    /// For a series: a list of season numbers, or "all". Ignored for a film.
    #[serde(default)]
    pub seasons: Option<Seasons>,
}

fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn day(iso: &str) -> String {
    clean(iso, 10)
}

fn title_year(d: &Details) -> String {
    let title = clean(&d.title, MAX_TITLE);
    match &d.year {
        Some(y) => format!("« {title} » ({})", clean(y, 4)),
        None => format!("« {title} »"),
    }
}

/// A search hit's titles: the person's language first, then the other one, then the original when it
/// is neither. TMDB matches a query against every title but shows one, so without the others a person
/// asking by the other language's title could not recognise the hit.
fn titles_year(mine: &Details, other: &Details) -> String {
    let title = clean(&mine.title, MAX_TITLE);
    let other_title = clean(&other.title, MAX_TITLE);
    let original = clean(&mine.original_title, MAX_TITLE);
    let mut out = format!("« {title} »");
    if !other_title.is_empty() && other_title != title {
        out.push_str(&format!(" / « {other_title} »"));
    }
    let mut notes = Vec::new();
    if !original.is_empty() && original != title && original != other_title {
        notes.push(format!("original « {original} »"));
    }
    if let Some(y) = &mine.year {
        notes.push(clean(y, 4));
    }
    if !notes.is_empty() {
        out.push_str(&format!(" ({})", notes.join(", ")));
    }
    out
}

fn list(seasons: &[u32]) -> String {
    seasons
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// ", season 3" / ", seasons 2, 3" in English; ", saison 3" / ", saisons 2, 3" in French.
fn seasons_phrase(seasons: &[u32], one: &str, many: &str) -> String {
    match seasons {
        [] => String::new(),
        [n] => format!(", {one} {n}"),
        _ => format!(", {many} {}", list(seasons)),
    }
}

/// "requested by X on YYYY-MM-DD", from the newest request Seerr holds.
fn requested_by(media: &MediaInfo) -> String {
    media
        .requests
        .iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
        .map_or_else(
            || "requested".to_string(),
            |r| {
                format!(
                    "requested by {} on {}",
                    clean(&r.requested_by, MAX_NAME),
                    day(&r.created_at)
                )
            },
        )
}

/// Counts a fault and words it for the model. An answer variant out of place (a 409 on a GET, say)
/// is a response this client does not understand, so it counts as `unexpected`.
fn fault(metrics: &Metrics, e: &SeerrError) -> String {
    let kind = match e {
        SeerrError::Unreachable => "unreachable",
        SeerrError::Http5xx(_) => "http_5xx",
        SeerrError::PermissionDenied => "permission_denied",
        _ => "unexpected",
    };
    metrics.inc(SEERR_FAILURES, kind);
    tracing::warn!(?e, "seerr call failed");
    match e {
        SeerrError::PermissionDenied => {
            "Seerr refused: this account may not request this. Nothing was requested.".to_string()
        }
        _ => format!("Seerr is not answering properly right now ({kind}). Try again later."),
    }
}

fn availability(media: Option<&MediaInfo>) -> String {
    match media.map(|m| m.status) {
        Some(5) => "available".to_string(),
        Some(4) => "partly available".to_string(),
        Some(2 | 3) => requested_by(media.expect("matched Some")),
        Some(6) => "blocklisted, cannot be requested".to_string(),
        _ => "not requested yet".to_string(),
    }
}

/// The query without a trailing year, "Title 2024" or "Title (2024)"; None when it has none, or is
/// nothing but one ("1917").
fn without_year(query: &str) -> Option<&str> {
    let q = query.trim_end();
    let (head, digits) = match q.strip_suffix(')') {
        Some(inner) => inner.rsplit_once('(')?,
        None => q.rsplit_once(' ')?,
    };
    let head = head.trim_end();
    let is_year = digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit());
    (is_year && !head.is_empty()).then_some(head)
}

/// # Errors
/// A Seerr fault, counted, as text.
pub async fn search_media(
    seerr: &Seerr,
    metrics: &Metrics,
    who: &Identity,
    query: &str,
) -> Result<String, String> {
    let (user, lang) = (who.seerr_user_id, who.language);
    let other = match lang {
        Language::Fr => Language::En,
        Language::En => Language::Fr,
    };
    let mut hits = seerr
        .search(user, query, lang.code())
        .await
        .map_err(|e| fault(metrics, &e))?;
    // Seerr finds nothing once a year follows the title, even the right one ("Soldat Collins 2024"),
    // so a miss is retried once without it.
    if let (true, Some(bare)) = (hits.is_empty(), without_year(query)) {
        hits = seerr
            .search(user, bare, lang.code())
            .await
            .map_err(|e| fault(metrics, &e))?;
    }
    if hits.is_empty() {
        // Search matches every translation, so a different title or a single word is the next try.
        // seerr-mcp cannot know whether the model has web search, hence "if you can".
        return Ok(format!(
            "Nothing found for {:?}. The search matches whole words of a title in any language: \
             try the English or original title, or one distinctive word of it. If you can search \
             the web, look up the title TMDB uses.",
            clean(query, MAX_TITLE)
        ));
    }
    // Each hit's details in both languages, every call at once: six round trips cost one.
    let both = |hit: Option<(MediaType, u32)>| async move {
        let (media_type, id) = hit?;
        let (mine, theirs) = tokio::join!(
            seerr.details(user, media_type, id, lang.code()),
            seerr.details(user, media_type, id, other.code()),
        );
        Some((media_type, id, mine.and_then(|m| theirs.map(|t| (m, t)))))
    };
    // tokio::join! has a fixed arity, so a change to SEARCH_RESULTS must change this call too.
    const _: () = assert!(SEARCH_RESULTS == 3);
    let mut top = hits.into_iter().take(SEARCH_RESULTS);
    let (a, b, c) = tokio::join!(both(top.next()), both(top.next()), both(top.next()));
    let mut lines = Vec::new();
    for (media_type, id, found) in [a, b, c].into_iter().flatten() {
        let (d, other_d) = found.map_err(|e| fault(metrics, &e))?;
        let label = match media_type {
            MediaType::Movie => format!("film {}, tmdb {id}", titles_year(&d, &other_d)),
            MediaType::Tv => format!(
                "series {}, tmdb {id}, seasons {}",
                titles_year(&d, &other_d),
                list(&d.seasons)
            ),
        };
        lines.push(format!("- {label}: {}", availability(d.media.as_ref())));
    }
    Ok(lines.join("\n"))
}

/// Which of `wanted` seasons Seerr already covers (pending, processing, partly or fully available).
fn covered(media: Option<&MediaInfo>, season: u32) -> Option<u8> {
    let media = media?;
    if media.status == 5 {
        return Some(5);
    }
    media
        .seasons
        .iter()
        .find(|(n, status)| *n == season && (2..=5).contains(status))
        .map(|(_, status)| *status)
}

fn confirmation(d: &Details, media_type: MediaType, seasons: &[u32], lang: Language) -> String {
    let title = clean(&d.title, MAX_TITLE);
    let year = d
        .year
        .as_deref()
        .map(|y| format!(" ({})", clean(y, 4)))
        .unwrap_or_default();
    // No ✅/❌ legend: Hermes prints its own, in the person's language.
    match lang {
        Language::Fr => {
            let kind = match media_type {
                MediaType::Movie => "film",
                MediaType::Tv => "série",
            };
            format!(
                "Demander « {title} »{year}, {kind}{} ?",
                seasons_phrase(seasons, "saison", "saisons")
            )
        }
        Language::En => {
            let kind = match media_type {
                MediaType::Movie => "film",
                MediaType::Tv => "series",
            };
            format!(
                "Request \"{title}\"{year}, {kind}{}?",
                seasons_phrase(seasons, "season", "seasons")
            )
        }
    }
}

/// The seasons to file, or why nothing will be asked. `Ok(Err(text))` is an answer for the user
/// (already covered); `Err(text)` is a bad argument.
fn seasons_to_file(
    d: &Details,
    wanted: Option<&Seasons>,
    metrics: &Metrics,
) -> Result<Result<Vec<u32>, String>, String> {
    let wanted = match wanted {
        None => {
            return Err(format!(
                "Say which seasons of {} to request: {}, or \"all\".",
                title_year(d),
                list(&d.seasons)
            ))
        }
        Some(Seasons::All(_)) => d.seasons.clone(),
        Some(Seasons::List(list_)) => {
            if let Some(bad) = list_.iter().find(|n| !d.seasons.contains(n)) {
                return Err(format!(
                    "{} has no season {bad}; its seasons are {}.",
                    title_year(d),
                    list(&d.seasons)
                ));
            }
            list_.clone()
        }
    };
    if wanted.is_empty() {
        return Err(format!(
            "Say which seasons of {} to request.",
            title_year(d)
        ));
    }
    let open: Vec<u32> = wanted
        .iter()
        .copied()
        .filter(|n| covered(d.media.as_ref(), *n).is_none())
        .collect();
    if !open.is_empty() {
        return Ok(Ok(open));
    }
    let all_available = wanted
        .iter()
        .all(|n| matches!(covered(d.media.as_ref(), *n), Some(4 | 5)));
    Ok(Err(already(d, all_available, metrics)))
}

fn already(d: &Details, available: bool, metrics: &Metrics) -> String {
    if available {
        metrics.inc(REQUEST_SKIPPED, "already_available");
        return format!("{} is already available on Jellyfin.", title_year(d));
    }
    metrics.inc(REQUEST_SKIPPED, "duplicate");
    let by = d
        .media
        .as_ref()
        .map_or_else(|| "requested".to_string(), requested_by);
    format!("{} was already {by}.", title_year(d))
}

/// `can_ask`: the client declared form elicitation. `ask` shows the confirmation and returns the
/// answer. `cancelled` is read after an accept, so a late tap cannot file a request the user was
/// already told had failed.
///
/// # Errors
/// A bad argument, or a Seerr fault (counted), as text.
pub async fn request_media<F, Fut>(
    seerr: &Seerr,
    metrics: &Metrics,
    who: &Identity,
    args: RequestArgs,
    can_ask: bool,
    ask: F,
    cancelled: impl Fn() -> bool,
) -> Result<String, String>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Answer>,
{
    if !can_ask {
        metrics.inc(REQUEST_FAILURES, "elicitation_unsupported");
        return Ok(
            "This chat cannot ask for a confirmation, so nothing was requested. \
                   Use Seerr's web page instead."
                .to_string(),
        );
    }
    let user = who.seerr_user_id;
    let lang = who.language.code();
    let d = seerr
        .details(user, args.media_type, args.tmdb_id, lang)
        .await
        .map_err(|e| fault(metrics, &e))?;

    let seasons = match args.media_type {
        MediaType::Movie => match d.media.as_ref().map(|m| m.status) {
            Some(5) => return Ok(already(&d, true, metrics)),
            Some(2..=4) => return Ok(already(&d, false, metrics)),
            Some(6) => {
                return Ok(format!(
                    "{} is blocklisted in Seerr and cannot be requested.",
                    title_year(&d)
                ))
            }
            _ => Vec::new(),
        },
        MediaType::Tv => match seasons_to_file(&d, args.seasons.as_ref(), metrics)? {
            Ok(open) => open,
            Err(answer) => return Ok(answer),
        },
    };

    match ask(confirmation(&d, args.media_type, &seasons, who.language)).await {
        Answer::Accepted if !cancelled() => {}
        Answer::Declined => {
            metrics.inc(REQUEST_SKIPPED, "declined");
            return Ok(not_requested(who.language).to_string());
        }
        // No answer in time, or an accept that arrived after the call was abandoned.
        Answer::Accepted | Answer::NoAnswer => {
            metrics.inc(REQUEST_FAILURES, "timeout");
            return Ok(not_requested(who.language).to_string());
        }
    }

    let filed = seerr
        .request(user, args.media_type, args.tmdb_id, Some(&seasons))
        .await;
    match filed {
        Ok(()) => Ok(format!(
            "Requested {}{}.",
            title_year(&d),
            seasons_phrase(&seasons, "season", "seasons")
        )),
        Err(SeerrError::Duplicate) => {
            // Someone filed between the re-read and the POST: read back who.
            let again = seerr
                .details(user, args.media_type, args.tmdb_id, lang)
                .await
                .unwrap_or(d);
            Ok(already(&again, false, metrics))
        }
        Err(SeerrError::NothingToRequest) => Ok(already(&d, true, metrics)),
        Err(SeerrError::Quota) => {
            metrics.inc(REQUEST_SKIPPED, "quota");
            Ok("Not requested: your Seerr quota is used up for now.".to_string())
        }
        Err(e) => Err(fault(metrics, &e)),
    }
}

/// # Errors
/// A Seerr fault, counted, as text.
pub async fn my_requests(
    seerr: &Seerr,
    metrics: &Metrics,
    who: &Identity,
) -> Result<String, String> {
    let user = who.seerr_user_id;
    let rows = seerr
        .user_requests(user, MY_REQUESTS)
        .await
        .map_err(|e| fault(metrics, &e))?;
    if rows.is_empty() {
        return Ok("No requests yet.".to_string());
    }
    let mut lines = Vec::new();
    for r in rows {
        let d = seerr
            .details(user, r.media_type, r.tmdb_id, who.language.code())
            .await
            .map_err(|e| fault(metrics, &e))?;
        // Three statuses, plus "declined": calling a refused request "requested"
        // would have the user waiting for something that will never come.
        let status = match (r.request_status, r.media_status) {
            (3, _) => "declined",
            (_, 5) => "ready on Jellyfin",
            (_, 3 | 4) => "downloading",
            _ => "requested",
        };
        lines.push(format!(
            "- {}, {}: {status}{}",
            title_year(&d),
            day(&r.created_at),
            match r.seasons.as_slice() {
                [] => String::new(),
                s => format!(
                    " ({})",
                    seasons_phrase(s, "season", "seasons").trim_start_matches(", ")
                ),
            }
        ));
    }
    Ok(lines.join("\n"))
}

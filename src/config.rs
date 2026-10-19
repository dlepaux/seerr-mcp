//! Startup configuration. Every secret is required and non-empty: seerr-mcp refuses to start rather
//! than serve `/mcp` or `/notify` open.

use std::collections::HashSet;
use std::num::NonZeroU32;

use serde::Deserialize;

#[derive(Debug)]
pub struct ConfigError(String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn err(message: impl Into<String>) -> ConfigError {
    ConfigError(message.into())
}

/// A Seerr user id that cannot be zero or absent. A Seerr call without `X-API-User` runs as the admin
/// (user 1 by key), so every call takes one of these by type (see `seerr::Seerr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SeerrUserId(NonZeroU32);

impl SeerrUserId {
    #[must_use]
    pub fn new(id: NonZeroU32) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

pub use crate::locale::Language;

/// One person: who they are to Hermes, to Seerr, to Jellyfin and to Matrix.
#[derive(Debug, Clone)]
pub struct Identity {
    pub profile: String,
    pub token_sha256: [u8; 32],
    pub seerr_user_id: SeerrUserId,
    /// Lowercase, without dashes: Seerr's webhook and Jellyfin do not agree on GUID formatting.
    pub jellyfin_user_id: String,
    pub matrix_room_id: Option<String>,
    pub language: Language,
    /// The household's other language, for a search hit's second title: the first language another
    /// person in the identities file uses. None in a one-language household.
    pub second_language: Option<Language>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIdentity {
    profile: String,
    token_sha256: String,
    seerr_user_id: SeerrUserId,
    jellyfin_user_id: String,
    #[serde(default)]
    matrix_room_id: Option<String>,
    language: Language,
}

#[must_use]
pub fn normalise_jellyfin_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn decode_hash(raw: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(raw, &mut out).ok()?;
    Some(out)
}

/// The profile becomes a URL path segment (`/p/<profile>/webhooks/ready-<profile>`), so it is a slug.
fn is_slug(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// # Errors
/// Malformed YAML, an invalid field, an empty list, or a duplicate profile, token or Jellyfin id.
pub fn parse_identities(yaml: &str) -> Result<Vec<Identity>, ConfigError> {
    let raw: Vec<RawIdentity> =
        serde_norway::from_str(yaml).map_err(|e| err(format!("identities: {e}")))?;
    if raw.is_empty() {
        return Err(err(
            "identities: the list is empty, nobody could use the server",
        ));
    }
    let languages: Vec<Language> = raw.iter().map(|r| r.language).collect();
    let (mut profiles, mut tokens, mut jellyfin) = (HashSet::new(), HashSet::new(), HashSet::new());
    raw.into_iter()
        .map(|r| {
            if !is_slug(&r.profile) {
                return Err(err(format!(
                    "identities: profile {:?} is not [a-z0-9_-]{{1,32}}",
                    r.profile
                )));
            }
            let token_sha256 = decode_hash(&r.token_sha256).ok_or_else(|| {
                err(format!(
                    "identities: {}'s token_sha256 is not 64 hex chars",
                    r.profile
                ))
            })?;
            let jellyfin_user_id = normalise_jellyfin_id(&r.jellyfin_user_id);
            if jellyfin_user_id.is_empty() {
                return Err(err(format!(
                    "identities: {}'s jellyfin_user_id is empty",
                    r.profile
                )));
            }
            if !profiles.insert(r.profile.clone())
                || !tokens.insert(token_sha256)
                || !jellyfin.insert(jellyfin_user_id.clone())
            {
                return Err(err(format!(
                    "identities: {} duplicates another entry",
                    r.profile
                )));
            }
            Ok(Identity {
                profile: r.profile,
                token_sha256,
                seerr_user_id: r.seerr_user_id,
                jellyfin_user_id,
                matrix_room_id: r.matrix_room_id.filter(|id| !id.is_empty()),
                language: r.language,
                second_language: languages.iter().copied().find(|l| *l != r.language),
            })
        })
        .collect()
}

#[derive(Debug)]
pub struct Config {
    pub addr: String,
    pub seerr_url: String,
    pub seerr_api_key: String,
    pub identities: Vec<Identity>,
    pub notify_secret: String,
    pub hermes_webhook_base: String,
    pub hermes_webhook_secret: String,
}

impl Config {
    /// `get` is the environment (`std::env::var(..).ok()` in `main`), injected so tests need no globals.
    ///
    /// # Errors
    /// Any required variable unset or empty, or an unreadable or invalid identities file.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let required = |key: &str| {
            get(key)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| err(format!("{key} is unset or empty; refusing to start")))
        };
        let path = required("SEERR_MCP_IDENTITIES")?;
        let yaml = std::fs::read_to_string(&path).map_err(|e| err(format!("{path}: {e}")))?;
        Ok(Self {
            addr: get("SEERR_MCP_ADDR")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "0.0.0.0:8080".to_string()),
            seerr_url: required("SEERR_URL")?.trim_end_matches('/').to_string(),
            seerr_api_key: required("SEERR_API_KEY")?,
            identities: parse_identities(&yaml)?,
            notify_secret: required("SEERR_MCP_NOTIFY_SECRET")?,
            hermes_webhook_base: required("HERMES_WEBHOOK_BASE")?
                .trim_end_matches('/')
                .to_string(),
            hermes_webhook_secret: required("HERMES_WEBHOOK_SECRET")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_A: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const TOKEN_B: &str = "60303ae22b998861bce3b28f33eec1be758a213c86c93c076dbe9f558c11c752";

    fn two_people() -> String {
        format!(
            "- profile: bob\n  token_sha256: {TOKEN_A}\n  seerr_user_id: 1\n  jellyfin_user_id: 5F2B-AA\n  matrix_room_id: '!dm:example.org'\n  language: en\n\
             - profile: alice\n  token_sha256: {TOKEN_B}\n  seerr_user_id: 7\n  jellyfin_user_id: bb11\n  language: fr\n"
        )
    }

    #[test]
    fn parses_the_spec_fields() {
        let ids = parse_identities(&two_people()).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].profile, "bob");
        assert_eq!(ids[0].seerr_user_id.get(), 1);
        assert_eq!(ids[0].matrix_room_id.as_deref(), Some("!dm:example.org"));
        assert_eq!(ids[1].language, Language::parse("fr").unwrap());
        assert_eq!(ids[1].matrix_room_id, None);
    }

    #[test]
    fn jellyfin_ids_are_normalised() {
        let ids = parse_identities(&two_people()).unwrap();
        assert_eq!(ids[0].jellyfin_user_id, "5f2baa");
    }

    #[test]
    fn refuses_seerr_user_zero() {
        // User 0 does not exist; an id that could be absent is how a call would run as the admin.
        let yaml = two_people().replace("seerr_user_id: 7", "seerr_user_id: 0");
        assert!(parse_identities(&yaml).is_err());
    }

    #[test]
    fn refuses_a_malformed_hash() {
        let yaml = two_people().replace(TOKEN_B, "abc");
        assert!(parse_identities(&yaml).is_err());
    }

    #[test]
    fn refuses_a_profile_unsafe_in_a_url_path() {
        let yaml = two_people().replace("profile: alice", "profile: ../x");
        assert!(parse_identities(&yaml).is_err());
    }

    #[test]
    fn refuses_duplicates() {
        let same_token = two_people().replace(TOKEN_B, TOKEN_A);
        assert!(parse_identities(&same_token).is_err(), "duplicate token");
        let same_profile = two_people().replace("profile: alice", "profile: bob");
        assert!(
            parse_identities(&same_profile).is_err(),
            "duplicate profile"
        );
        let same_jellyfin = two_people().replace("bb11", "5f2baa");
        assert!(
            parse_identities(&same_jellyfin).is_err(),
            "duplicate jellyfin id"
        );
    }

    #[test]
    fn the_second_language_is_another_person_s() {
        let (en, fr) = (Language::parse("en"), Language::parse("fr"));
        let ids = parse_identities(&two_people()).unwrap();
        assert_eq!((ids[0].second_language, ids[1].second_language), (fr, en));
        let alone =
            parse_identities(&two_people().replace("language: fr", "language: en")).unwrap();
        assert_eq!(alone[0].second_language, None);
    }

    #[test]
    fn an_unknown_language_names_the_ones_this_build_has() {
        let e = parse_identities(&two_people().replace("language: fr", "language: tr"))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("unknown language \"tr\"; this build has en, fr"),
            "{e}"
        );
    }

    #[test]
    fn refuses_an_unknown_language_and_an_unknown_field() {
        assert!(parse_identities(&two_people().replace("language: fr", "language: tr")).is_err());
        assert!(parse_identities(
            &two_people().replace("language: fr", "language: fr\n  admin: true")
        )
        .is_err());
    }

    #[test]
    fn refuses_an_empty_list() {
        assert!(parse_identities("[]").is_err());
    }

    fn env_with(identities_path: &str) -> impl Fn(&str) -> Option<String> + '_ {
        move |key| {
            Some(
                match key {
                    "SEERR_URL" => "https://seerr.example.org",
                    "SEERR_API_KEY" => "k",
                    "SEERR_MCP_IDENTITIES" => identities_path,
                    "SEERR_MCP_NOTIFY_SECRET" => "n",
                    "HERMES_WEBHOOK_BASE" => "http://hermes:8644",
                    "HERMES_WEBHOOK_SECRET" => "h",
                    _ => return None,
                }
                .to_string(),
            )
        }
    }

    fn identities_file(name: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("seerr-mcp-{name}-{}.yml", std::process::id()));
        std::fs::write(&path, two_people()).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn loads_from_the_environment_with_the_default_address() {
        let path = identities_file("ok");
        let config = Config::from_lookup(env_with(&path)).unwrap();
        assert_eq!(config.addr, "0.0.0.0:8080");
        assert_eq!(config.seerr_url, "https://seerr.example.org");
        assert_eq!(config.identities.len(), 2);
    }

    #[test]
    fn every_secret_is_required_and_non_empty() {
        let path = identities_file("secrets");
        for missing in [
            "SEERR_URL",
            "SEERR_API_KEY",
            "SEERR_MCP_IDENTITIES",
            "SEERR_MCP_NOTIFY_SECRET",
            "HERMES_WEBHOOK_BASE",
            "HERMES_WEBHOOK_SECRET",
        ] {
            let unset = |k: &str| {
                if k == missing {
                    None
                } else {
                    env_with(&path)(k)
                }
            };
            assert!(
                Config::from_lookup(unset).is_err(),
                "{missing} unset must refuse to start"
            );
            let empty = |k: &str| {
                if k == missing {
                    Some(String::new())
                } else {
                    env_with(&path)(k)
                }
            };
            assert!(
                Config::from_lookup(empty).is_err(),
                "{missing} empty must refuse to start"
            );
        }
    }
}

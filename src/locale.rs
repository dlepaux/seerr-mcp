//! Every sentence a person reads, per language, from `locales/<code>.json`: flat `"key": "template"`
//! pairs with `{name}` placeholders, `en.json` the reference. The files are compiled in, so the image
//! needs no mount. There is no fallback: a key missing from a locale fails the completeness test below,
//! it never shows English at runtime. Adding a language is one file plus one line in `LOCALES`.
//!
//! Optional clauses: when the clause is simply absent, the template holds a fragment placeholder and
//! the clause's wording is its own key (`{with_year}` / `with_year`, rendered by `fragment`). When the
//! two forms are worded differently, they are two whole keys. Plurals are `key_one` / `key_other`.
//!
//! Text the model reads (tool descriptions, tool results), logs and metrics stay in Rust, in English.

#[cfg(test)]
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{de::Error as _, Deserialize, Deserializer};

/// The code is also what Seerr's `language` query parameter takes, so it is an ISO 639-1 code.
const LOCALES: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.json")),
    ("fr", include_str!("../locales/fr.json")),
];

type Table = HashMap<String, String>;

/// # Panics
/// On a locale file that is not a flat object of strings, a defect the tests catch before release.
fn tables() -> &'static HashMap<&'static str, Table> {
    static TABLES: OnceLock<HashMap<&'static str, Table>> = OnceLock::new();
    TABLES.get_or_init(|| {
        LOCALES
            .iter()
            .map(|(code, json)| {
                let table = serde_json::from_str(json)
                    .unwrap_or_else(|e| panic!("locales/{code}.json: {e}"));
                (*code, table)
            })
            .collect()
    })
}

fn codes() -> String {
    LOCALES
        .iter()
        .map(|(code, _)| *code)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A language this build has a locale for. Only `parse` makes one, so an unknown code fails at
/// config load, never at the first message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language(&'static str);

impl Language {
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        LOCALES
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(c, _)| Self(c))
    }

    #[must_use]
    pub fn code(self) -> &'static str {
        self.0
    }

    /// # Panics
    /// On a key this locale lacks: a defect, which the completeness test and the message tests catch.
    #[must_use]
    pub fn text(self, key: &str) -> &'static str {
        tables()[self.0]
            .get(key)
            .unwrap_or_else(|| panic!("locales/{}.json has no {key:?}", self.0))
    }

    #[must_use]
    pub fn render(self, key: &str, args: &[(&str, &str)]) -> String {
        fill(self.text(key), args)
    }

    /// An optional clause: `key` rendered with `name` = the value when there is one, "" when not.
    #[must_use]
    pub fn fragment(self, key: &str, name: &str, value: Option<&str>) -> String {
        value
            .map(|v| self.render(key, &[(name, v)]))
            .unwrap_or_default()
    }

    /// `key_one` or `key_other` for a count of `n`. French counts 0 as singular; a language with more
    /// forms extends this match and adds its keys.
    #[must_use]
    pub fn plural(self, key: &str, n: usize) -> String {
        let one = match self.0 {
            "fr" => n <= 1,
            _ => n == 1,
        };
        format!("{key}_{}", if one { "one" } else { "other" })
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let code = String::deserialize(d)?;
        Self::parse(&code).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown language {code:?}; this build has {}",
                codes()
            ))
        })
    }
}

/// Substitutes `{name}` in one pass, so a value that itself holds `{…}` (a title is untrusted) is
/// never expanded. Each turn consumes at least one byte, so the loop is bounded by the template.
fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let found = after.find('}').and_then(|end| {
            let name = &after[..end];
            args.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v, end))
        });
        match found {
            Some((value, end)) => {
                out.push_str(value);
                rest = &after[end + 1..];
            }
            None => {
                debug_assert!(false, "an unfilled placeholder in {template:?}");
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `{name}` placeholders of a template.
#[cfg(test)]
fn placeholders(template: &str) -> BTreeSet<&str> {
    template
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A contributor's safety net: every locale has exactly the reference's keys, each with the
    /// reference's placeholders, and no empty value. Every problem is listed, not just the first.
    #[test]
    fn every_locale_matches_the_reference() {
        let reference = &tables()["en"];
        let mut problems = Vec::new();
        for (code, table) in tables() {
            for key in reference.keys().filter(|k| !table.contains_key(*k)) {
                problems.push(format!("{code}.json lacks {key:?}"));
            }
            for (key, value) in table {
                let Some(want) = reference.get(key) else {
                    problems.push(format!("{code}.json has {key:?}, which en.json does not"));
                    continue;
                };
                if value.trim().is_empty() {
                    problems.push(format!("{code}.json: {key:?} is empty"));
                }
                if placeholders(value) != placeholders(want) {
                    problems.push(format!(
                        "{code}.json: {key:?} has placeholders {:?}, en.json has {:?}",
                        placeholders(value),
                        placeholders(want)
                    ));
                }
            }
        }
        problems.sort();
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }

    #[test]
    fn codes_are_unique_and_the_reference_exists() {
        assert_eq!(tables().len(), LOCALES.len(), "a code is listed twice");
        assert!(Language::parse("en").is_some());
        assert!(Language::parse("tr").is_none());
    }

    #[test]
    fn a_value_is_never_expanded_as_a_template() {
        let filled = fill(
            "« {title} »{with_year}",
            &[("title", "{with_year}"), ("with_year", " (2024)")],
        );
        assert_eq!(filled, "« {with_year} » (2024)");
    }

    #[test]
    fn plural_forms() {
        let (en, fr) = (
            Language::parse("en").unwrap(),
            Language::parse("fr").unwrap(),
        );
        assert_eq!(en.plural("k", 1), "k_one");
        assert_eq!(en.plural("k", 0), "k_other");
        assert_eq!(en.plural("k", 2), "k_other");
        assert_eq!(fr.plural("k", 0), "k_one");
        assert_eq!(fr.plural("k", 2), "k_other");
    }

    #[test]
    fn an_absent_fragment_renders_nothing() {
        let en = Language::parse("en").unwrap();
        assert_eq!(en.fragment("with_year", "year", Some("2024")), " (2024)");
        assert_eq!(en.fragment("with_year", "year", None), "");
    }
}

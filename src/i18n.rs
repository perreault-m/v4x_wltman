//! Minimal, dependency-free internationalization for user-facing strings.
//!
//! Design goals, in order:
//! 1. Adding a new language should mean "add one JSON file + one enum
//!    variant", nothing more.
//! 2. No new external dependency -- this app already parses JSON
//!    (`serde_json`) and that's all a flat key/value translation catalog
//!    needs. A wallet manager's UI has no plural/gender rules to worry
//!    about, so a full i18n framework (Fluent, ICU MessageFormat, ...)
//!    would be more machinery than the problem calls for.
//! 3. Translation catalogs are embedded into the binary at compile time
//!    (`include_str!`), not loaded from disk at runtime: a missing/corrupt
//!    locale file must never be something a user can hit in the wild. See
//!    `locales/README.md` for how to add a language.
//! 4. A missing key must never panic or silently render blank -- it should
//!    fall back to the reference language (French, currently the only
//!    complete catalog), and if it's missing there too, render as an
//!    obviously-wrong placeholder (`"??key??"`) so it's impossible to miss
//!    during review, rather than a blank label a user might not notice.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-22

use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

/// A supported UI language. Adding a new one:
/// 1. Create `locales/xx.json` (copy `locales/en.json` as a starting point).
/// 2. Add a variant here.
/// 3. Add a match arm in `code`, `label`, `all`, `fallback`, and `raw_json`.
/// That's it -- no other code needs to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    #[default]
    Fr,
    En,
    Es,
}

impl Lang {
    /// Short code (used for persistence/display), e.g. `"fr"`.
    pub fn code(&self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
            Lang::Es => "es",
        }
    }

    /// Human-readable name, as shown in the language picker.
    pub fn label(&self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
            Lang::Es => "Español",
        }
    }

    /// All supported languages, for populating a picker.
    pub fn all() -> &'static [Lang] {
        &[Lang::Fr, Lang::En, Lang::Es]
    }

    /// The catalog this language falls back to when a key is missing.
    /// French is the reference language (currently the only guaranteed to
    /// be complete, since it's what the app originally shipped with) --
    /// every other language falls back to it. French itself has no
    /// fallback: if a key is missing there, it's a real bug, not a
    /// translation gap.
    fn fallback(&self) -> Option<Lang> {
        match self {
            Lang::Fr => None,
            Lang::En => Some(Lang::Fr),
            Lang::Es => Some(Lang::Fr),
        }
    }

    fn raw_json(&self) -> &'static str {
        match self {
            Lang::Fr => include_str!("locales/fr.json"),
            Lang::En => include_str!("locales/en.json"),
            Lang::Es => include_str!("locales/es.json"),
        }
    }
}

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Parsed catalogs, one `HashMap<key, value>` per language, built once on
/// first use and cached for the process's lifetime.
fn catalogs() -> &'static HashMap<Lang, HashMap<String, String>> {
    static CATALOGS: OnceLock<HashMap<Lang, HashMap<String, String>>> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        Lang::all()
            .iter()
            .map(|&lang| (lang, parse_catalog(lang)))
            .collect()
    })
}

/// Parses one language's embedded JSON into a flat key/value map.
///
/// Panics on malformed JSON -- deliberately: a broken catalog is a build-time
/// bug (these files are embedded, not user-supplied), and it's far better to
/// fail loudly at startup than to silently show blank/garbled UI text.
fn parse_catalog(lang: Lang) -> HashMap<String, String> {
    let value: Value = serde_json::from_str(lang.raw_json())
        .unwrap_or_else(|e| panic!("locales/{}.json is not valid JSON: {}", lang.code(), e));

    let Value::Object(map) = value else {
        panic!("locales/{}.json must be a flat JSON object", lang.code());
    };

    map.into_iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => Some((k, s)),
            _ => {
                eprintln!(
                    "locales/{}.json: key \"{}\" is not a string, ignoring it",
                    lang.code(),
                    k
                );
                None
            }
        })
        .collect()
}

/// Looks up `key` in `lang`'s catalog, falling back to the reference
/// language, and finally to an obviously-wrong placeholder if truly missing
/// everywhere.
pub fn t(lang: Lang, key: &str) -> String {
    let catalogs = catalogs();

    if let Some(value) = catalogs.get(&lang).and_then(|c| c.get(key)) {
        return value.clone();
    }
    if let Some(fallback) = lang.fallback() {
        if let Some(value) = catalogs.get(&fallback).and_then(|c| c.get(key)) {
            return value.clone();
        }
    }

    format!("??{}??", key)
}

/// Same as [`t`], but substitutes `{name}`-style placeholders in the
/// resolved string with the given values, e.g.
/// `t_args(lang, "send.progress", &[("amount", "25"), ("network", "testnet")])`
/// for a catalog entry like `"Sending {amount} XRP on {network}..."`.
pub fn t_args(lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
    let mut result = t(lang, key);
    for (name, value) in args {
        result = result.replace(&format!("{{{}}}", name), value);
    }
    result
}
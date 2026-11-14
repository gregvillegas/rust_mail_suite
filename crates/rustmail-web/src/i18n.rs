//! Localisation (Fluent).
//!
//! Message files live in `locales/<code>/main.ftl` and are embedded in the
//! binary at compile time. English is the source language and the fallback:
//! a key missing from another locale falls back to English instead of
//! rendering blank.
//!
//! Adding a language: create `locales/<code>/main.ftl` with every key from the
//! English file, then add a variant to [`Locale`]. The tests fail until the new
//! file is complete.
//!
//! Locale selection per request, in order: the `rm_lang` cookie (an explicit
//! choice), the `Accept-Language` header, then English. A user's stored profile
//! locale is wired in when user settings arrive in Phase 7.

use std::borrow::Cow;
use std::collections::HashMap;

use fluent_templates::fluent_bundle::FluentValue;
use fluent_templates::{langid, static_loader, LanguageIdentifier, Loader};

static_loader! {
    static LOCALES = {
        locales: "./locales",
        fallback_language: "en",
        // Bidi isolation marks around placeables are meant for mixed-direction
        // text and show up as stray characters in HTML attributes and tests.
        customise: |bundle| bundle.set_use_isolating(false),
    };
}

/// Name of the cookie holding an explicit language choice.
pub const LANG_COOKIE: &str = "rm_lang";

/// A language the UI is available in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    /// English (source language and fallback).
    En,
    /// German.
    De,
}

impl Locale {
    /// Every supported locale, in the order shown in the language menu.
    pub const ALL: &'static [Locale] = &[Locale::En, Locale::De];

    /// The language used when nothing else matches.
    pub const DEFAULT: Locale = Locale::En;

    /// The BCP 47 code, used for `<html lang>` and the language cookie.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::De => "de",
        }
    }

    /// The language's name in that language, for the language menu.
    #[must_use]
    pub fn endonym(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::De => "Deutsch",
        }
    }

    /// Parses a code such as `de` or `de-AT` (only the primary subtag counts).
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        let primary = code.trim().split(['-', '_']).next()?.to_ascii_lowercase();
        Self::ALL.iter().copied().find(|l| l.code() == primary)
    }

    fn lang_id(self) -> LanguageIdentifier {
        match self {
            Self::En => langid!("en"),
            Self::De => langid!("de"),
        }
    }

    /// Picks the best supported locale for an `Accept-Language` header value.
    ///
    /// Honours `q` weights and ignores wildcards and `q=0` entries.
    #[must_use]
    pub fn negotiate(accept_language: Option<&str>) -> Self {
        let Some(header) = accept_language else {
            return Self::DEFAULT;
        };
        let mut candidates: Vec<(f32, usize, &str)> = header
            .split(',')
            .enumerate()
            .filter_map(|(order, part)| {
                let mut pieces = part.split(';');
                let tag = pieces.next()?.trim();
                if tag.is_empty() || tag == "*" {
                    return None;
                }
                let q = pieces
                    .find_map(|p| p.trim().strip_prefix("q="))
                    .map_or(Some(1.0), |v| v.trim().parse::<f32>().ok())?;
                (q > 0.0).then_some((q, order, tag))
            })
            .collect();
        // Highest weight first; header order breaks ties.
        candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        candidates
            .into_iter()
            .find_map(|(_, _, tag)| Self::from_code(tag))
            .unwrap_or(Self::DEFAULT)
    }
}

/// Looks up messages for one locale.
#[derive(Debug, Clone, Copy)]
pub struct Translator {
    locale: Locale,
}

impl Translator {
    /// Creates a translator for `locale`.
    #[must_use]
    pub fn new(locale: Locale) -> Self {
        Self { locale }
    }

    /// The message for `key`. A key that exists in no locale is logged and
    /// rendered as the key itself, which is visible enough to get fixed.
    #[must_use]
    pub fn t(&self, key: &str) -> String {
        self.lookup(key, None)
    }

    /// The message for `key` with one string argument.
    #[must_use]
    pub fn with_str(&self, key: &str, name: &'static str, value: &str) -> String {
        self.lookup(key, Some(&args(name, FluentValue::from(value.to_owned()))))
    }

    /// The message for `key` with one numeric argument (selectors pick plurals).
    #[must_use]
    pub fn with_num(&self, key: &str, name: &'static str, value: i64) -> String {
        self.lookup(key, Some(&args(name, FluentValue::from(value))))
    }

    fn lookup(
        &self,
        key: &str,
        arguments: Option<&HashMap<Cow<'static, str>, FluentValue<'static>>>,
    ) -> String {
        let id = self.locale.lang_id();
        match LOCALES.try_lookup_complete(&id, key, arguments) {
            Some(text) => text,
            None => {
                tracing::warn!(key, locale = self.locale.code(), "missing translation");
                key.to_owned()
            }
        }
    }
}

fn args(
    name: &'static str,
    value: FluentValue<'static>,
) -> HashMap<Cow<'static, str>, FluentValue<'static>> {
    HashMap::from([(Cow::Borrowed(name), value)])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    fn crate_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// Message ids defined in a `.ftl` file (top-level `id =` lines).
    fn message_ids(ftl: &str) -> BTreeSet<String> {
        ftl.lines()
            .filter(|l| !l.starts_with([' ', '\t', '#', '-']))
            .filter_map(|l| l.split_once('='))
            .map(|(id, _)| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect()
    }

    fn locale_ids(code: &str) -> BTreeSet<String> {
        let path = crate_dir().join("locales").join(code).join("main.ftl");
        message_ids(
            &fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
    }

    #[test]
    fn every_locale_defines_exactly_the_english_keys() {
        let english = locale_ids("en");
        assert!(english.len() > 50, "english file looks truncated");
        for locale in Locale::ALL {
            let ids = locale_ids(locale.code());
            let missing: Vec<_> = english.difference(&ids).collect();
            let extra: Vec<_> = ids.difference(&english).collect();
            assert!(
                missing.is_empty(),
                "{} is missing keys: {missing:?}",
                locale.code()
            );
            assert!(
                extra.is_empty(),
                "{} has keys unknown to English: {extra:?}",
                locale.code()
            );
        }
    }

    fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files(&path, ext, out);
            } else if path.extension().is_some_and(|e| e == ext) {
                out.push(path);
            }
        }
    }

    /// Keys referenced from templates and Rust sources, found by scanning for
    /// `t("..")`, `with_str("..")`, `with_num("..")` style calls and `msg: ".."`.
    fn referenced_keys() -> BTreeSet<String> {
        let mut files = Vec::new();
        collect_files(&crate_dir().join("templates"), "html", &mut files);
        collect_files(&crate_dir().join("src"), "rs", &mut files);
        let mut keys = BTreeSet::new();
        for file in files {
            let source = fs::read_to_string(&file).expect("read source");
            // Test modules use deliberately fake keys; only production code counts.
            let text = source.split("#[cfg(test)]").next().unwrap_or(&source);
            for marker in [
                "t(\"",
                "t_str(\"",
                "t_num(\"",
                "with_str(\"",
                "with_num(\"",
                "key: \"",
            ] {
                for (idx, _) in text.match_indices(marker) {
                    // `t("` must be a whole identifier, not the tail of `format("`.
                    let before = text[..idx].chars().next_back();
                    if marker == "t(\"" && before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let rest = &text[idx + marker.len()..];
                    if let Some(end) = rest.find('"') {
                        let key = &rest[..end];
                        if key
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                            && key.contains('-')
                        {
                            keys.insert(key.to_owned());
                        }
                    }
                }
            }
        }
        keys
    }

    #[test]
    fn every_key_used_in_code_and_templates_exists() {
        let defined = locale_ids("en");
        let used = referenced_keys();
        assert!(
            used.len() > 20,
            "scanner found only {} keys; did the call syntax change?",
            used.len()
        );
        let unknown: Vec<_> = used.difference(&defined).collect();
        assert!(unknown.is_empty(), "keys used but not defined: {unknown:?}");
    }

    #[test]
    fn lookups_work_per_locale() {
        assert_eq!(Translator::new(Locale::En).t("login-submit"), "Sign in");
        assert_eq!(Translator::new(Locale::De).t("login-submit"), "Anmelden");
        assert_eq!(
            Translator::new(Locale::En).with_str("signed-in-as", "name", "Alice"),
            "Signed in as Alice"
        );
        assert_eq!(
            Translator::new(Locale::De).with_str("signed-in-as", "name", "Alice"),
            "Angemeldet als Alice"
        );
    }

    #[test]
    fn plural_selectors_follow_the_count() {
        let en = Translator::new(Locale::En);
        assert_eq!(
            en.with_num("dashboard-activity-count", "count", 1),
            "1 event"
        );
        assert_eq!(
            en.with_num("dashboard-activity-count", "count", 0),
            "0 events"
        );
        assert_eq!(
            en.with_num("dashboard-activity-count", "count", 7),
            "7 events"
        );
        let de = Translator::new(Locale::De);
        assert_eq!(
            de.with_num("dashboard-activity-count", "count", 1),
            "1 Ereignis"
        );
        assert_eq!(
            de.with_num("dashboard-activity-count", "count", 3),
            "3 Ereignisse"
        );
        assert_eq!(
            en.with_num("login-error-locked", "minutes", 1),
            "Too many failed attempts. Try again in 1 minute."
        );
        assert_eq!(
            en.with_num("login-error-locked", "minutes", 15),
            "Too many failed attempts. Try again in 15 minutes."
        );
    }

    #[test]
    fn unknown_keys_render_as_the_key() {
        assert_eq!(Translator::new(Locale::En).t("no-such-key"), "no-such-key");
    }

    #[test]
    fn no_bidi_isolation_marks_leak_into_output() {
        let text = Translator::new(Locale::En).with_str("signed-in-as", "name", "Alice");
        assert!(!text.contains(['\u{2068}', '\u{2069}']));
    }

    #[test]
    fn accept_language_negotiation() {
        let pick = |h: &str| Locale::negotiate(Some(h));
        assert_eq!(pick("de"), Locale::De);
        assert_eq!(pick("de-AT,de;q=0.9,en;q=0.5"), Locale::De);
        assert_eq!(pick("en-GB,en;q=0.9,de;q=0.8"), Locale::En);
        assert_eq!(
            pick("fr-CH, fr;q=0.9, de;q=0.8, en;q=0.7"),
            Locale::De,
            "first supported by weight"
        );
        assert_eq!(pick("en;q=0.2, de;q=0.9"), Locale::De, "weights beat order");
        assert_eq!(pick("fr, es"), Locale::DEFAULT);
        assert_eq!(pick("*"), Locale::DEFAULT);
        assert_eq!(pick("de;q=0"), Locale::DEFAULT, "q=0 means not acceptable");
        assert_eq!(pick(""), Locale::DEFAULT);
        assert_eq!(
            pick("de;q=abc, en"),
            Locale::En,
            "malformed weight is skipped"
        );
        assert_eq!(Locale::negotiate(None), Locale::DEFAULT);
    }

    #[test]
    fn locale_codes_parse_by_primary_subtag() {
        assert_eq!(Locale::from_code("de"), Some(Locale::De));
        assert_eq!(Locale::from_code("DE-at"), Some(Locale::De));
        assert_eq!(Locale::from_code("en_US"), Some(Locale::En));
        assert_eq!(Locale::from_code("xx"), None);
        assert_eq!(Locale::from_code(""), None);
        for locale in Locale::ALL {
            assert_eq!(Locale::from_code(locale.code()), Some(*locale));
        }
    }
}

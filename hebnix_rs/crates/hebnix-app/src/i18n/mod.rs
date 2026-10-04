//! central localization. every user-facing string goes through `t()`,
//! `t_args()` or `t_n()` with a stable key from locales/en/*.ftl.
//!
//! lookup order: active language -> english -> the key itself (logged once).
//! language is picked as: HEBNIX_LANG env > settings.language > system > en.
//! translators only touch locales/, see locales/README.md.
#![allow(dead_code)]

pub mod fonts;
pub mod format;
pub mod layout;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use serde::Deserialize;
use unic_langid::LanguageIdentifier;

include!(concat!(env!("OUT_DIR"), "/locales_embedded.rs"));

const LOCALES_TOML: &str = include_str!("../../locales/locales.toml");

pub const DEFAULT_LANG: &str = "en";
/// settings value meaning "follow the system language"
pub const AUTO: &str = "auto";
/// env var that overrides everything, handy for testing a locale
pub const ENV_OVERRIDE: &str = "HEBNIX_LANG";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// one row of locales/locales.toml
#[derive(Debug, Clone, Deserialize)]
pub struct LocaleMeta {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub direction: String,
    /// "reviewed" or "draft"
    #[serde(default)]
    pub status: String,
}

impl LocaleMeta {
    pub fn direction(&self) -> Direction {
        if self.direction.eq_ignore_ascii_case("rtl") {
            Direction::Rtl
        } else {
            Direction::Ltr
        }
    }

    pub fn is_draft(&self) -> bool {
        !self.status.eq_ignore_ascii_case("reviewed")
    }
}

#[derive(Deserialize)]
struct MetaFile {
    #[serde(default)]
    locale: Vec<LocaleMeta>,
}

pub fn locale_meta() -> Vec<LocaleMeta> {
    match toml::from_str::<MetaFile>(LOCALES_TOML) {
        Ok(file) => file.locale,
        Err(e) => {
            tracing::warn!("locales.toml is invalid: {e}");
            Vec::new()
        }
    }
}

struct Catalog {
    lang: String,
    bundle: FluentBundle<FluentResource>,
}

struct State {
    active: Catalog,
    english: Catalog,
    user_dir: Option<PathBuf>,
}

static STATE: OnceLock<RwLock<State>> = OnceLock::new();
static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn warn_once(what: String) {
    let set = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut set) = set.lock() {
        if set.insert(what.clone()) {
            tracing::warn!("i18n: {what}");
        }
    }
}

fn add_source(bundle: &mut FluentBundle<FluentResource>, source: String, origin: &str) {
    let resource = match FluentResource::try_new(source) {
        Ok(r) => r,
        Err((r, errors)) => {
            // keep whatever parsed, a typo shouldn't blank the whole locale
            warn_once(format!("{origin}: {} parse error(s)", errors.len()));
            r
        }
    };
    bundle.add_resource_overriding(resource);
}

fn build_catalog(code: &str, user_dir: Option<&Path>) -> Option<Catalog> {
    let langid: LanguageIdentifier = code.parse().ok()?;
    let mut bundle = FluentBundle::new_concurrent(vec![langid]);
    // the isolating marks show up as boxes in egui text
    bundle.set_use_isolating(false);

    let mut found = false;
    for (c, source) in EMBEDDED_FTL {
        if c.eq_ignore_ascii_case(code) {
            add_source(&mut bundle, (*source).to_string(), code);
            found = true;
        }
    }

    if let Some(dir) = user_dir {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join(code))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ftl"))
            .collect();
        files.sort();
        for file in files {
            match std::fs::read_to_string(&file) {
                Ok(text) => {
                    add_source(&mut bundle, text, &file.to_string_lossy());
                    found = true;
                }
                Err(e) => warn_once(format!("{}: {e}", file.display())),
            }
        }
    }

    found.then(|| Catalog {
        lang: code.to_string(),
        bundle,
    })
}

fn english_catalog(user_dir: Option<&Path>) -> Catalog {
    build_catalog(DEFAULT_LANG, user_dir).unwrap_or_else(|| {
        let langid: LanguageIdentifier = DEFAULT_LANG.parse().expect("en is a valid tag");
        let mut bundle = FluentBundle::new_concurrent(vec![langid]);
        bundle.set_use_isolating(false);
        Catalog {
            lang: DEFAULT_LANG.to_string(),
            bundle,
        }
    })
}

fn state() -> &'static RwLock<State> {
    STATE.get_or_init(|| {
        RwLock::new(State {
            active: english_catalog(None),
            english: english_catalog(None),
            user_dir: None,
        })
    })
}

/// call once at startup. `setting` is `settings.language` ("auto", "de", ...).
/// `user_dir` is an optional folder with locales/<code>/*.ftl that layer over
/// the built-in ones, so translators can test without rebuilding.
pub fn init(setting: &str, user_dir: Option<PathBuf>) -> String {
    if let Ok(mut s) = state().write() {
        s.user_dir = user_dir;
    }
    set_language(setting)
}

/// switch language now. returns the code that ended up active.
pub fn set_language(setting: &str) -> String {
    let user_dir = state().read().ok().and_then(|s| s.user_dir.clone());
    let codes: Vec<String> = available().into_iter().map(|l| l.code).collect();
    let code = resolve(setting, &codes);

    let active = if code == DEFAULT_LANG {
        english_catalog(user_dir.as_deref())
    } else {
        build_catalog(&code, user_dir.as_deref())
            .unwrap_or_else(|| english_catalog(user_dir.as_deref()))
    };
    let english = english_catalog(user_dir.as_deref());

    let applied = active.lang.clone();
    if let Ok(mut s) = state().write() {
        s.active = active;
        s.english = english;
    }
    applied
}

/// the language code in use right now
pub fn current() -> String {
    state()
        .read()
        .map(|s| s.active.lang.clone())
        .unwrap_or_else(|_| DEFAULT_LANG.to_string())
}

/// bcp47 tag for the active language, for things like DirectWrite
pub fn current_bcp47() -> String {
    current()
}

pub fn direction() -> Direction {
    let code = current();
    locale_meta()
        .into_iter()
        .find(|m| m.code.eq_ignore_ascii_case(&code))
        .map(|m| m.direction())
        .unwrap_or(Direction::Ltr)
}

/// every language that can be picked: built-in plus any folder in the user dir
pub fn available() -> Vec<LocaleMeta> {
    let user_dir = state().read().ok().and_then(|s| s.user_dir.clone());
    let mut out: Vec<LocaleMeta> = Vec::new();

    for meta in locale_meta() {
        if EMBEDDED_FTL
            .iter()
            .any(|(c, _)| c.eq_ignore_ascii_case(&meta.code))
        {
            out.push(meta);
        }
    }

    if let Some(dir) = user_dir {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Some(code) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !path.is_dir() || code.parse::<LanguageIdentifier>().is_err() {
                continue;
            }
            if out.iter().any(|m| m.code.eq_ignore_ascii_case(code)) {
                continue;
            }
            out.push(LocaleMeta {
                code: code.to_string(),
                name: code.to_string(),
                direction: String::new(),
                status: "draft".to_string(),
            });
        }
    }

    out
}

/// "Deutsch" or "Deutsch (draft)" style name for pickers
pub fn display_name(meta: &LocaleMeta) -> String {
    if meta.is_draft() && meta.code != DEFAULT_LANG {
        format!("{} ({})", meta.name, t("language-draft-suffix"))
    } else {
        meta.name.clone()
    }
}

fn normalize_tag(tag: &str) -> String {
    // "de_DE.UTF-8@euro" -> "de-DE"
    let tag = tag.split(['.', '@']).next().unwrap_or(tag);
    tag.replace('_', "-")
}

/// match a requested tag against the available codes: exact, then same
/// language (+script when both have one), then same language only
fn negotiate(wanted: &str, available: &[String]) -> Option<String> {
    let wanted = normalize_tag(wanted);
    if let Some(hit) = available.iter().find(|c| c.eq_ignore_ascii_case(&wanted)) {
        return Some(hit.clone());
    }

    let want: LanguageIdentifier = wanted.parse().ok()?;
    let parsed: Vec<(String, LanguageIdentifier)> = available
        .iter()
        .filter_map(|c| Some((c.clone(), c.parse().ok()?)))
        .collect();

    if let Some(script) = want.script {
        if let Some((code, _)) = parsed
            .iter()
            .find(|(_, id)| id.language == want.language && id.script == Some(script))
        {
            return Some(code.clone());
        }
    }
    parsed
        .iter()
        .find(|(_, id)| id.language == want.language && id.script.is_none() && id.region.is_none())
        .or_else(|| parsed.iter().find(|(_, id)| id.language == want.language))
        .map(|(code, _)| code.clone())
}

fn resolve(setting: &str, available: &[String]) -> String {
    let default = DEFAULT_LANG.to_string();

    if let Ok(forced) = std::env::var(ENV_OVERRIDE) {
        let forced = forced.trim();
        if !forced.is_empty() && !forced.eq_ignore_ascii_case(AUTO) {
            return negotiate(forced, available).unwrap_or(default);
        }
    }

    let setting = setting.trim();
    if !setting.is_empty() && !setting.eq_ignore_ascii_case(AUTO) {
        if let Some(code) = negotiate(setting, available) {
            return code;
        }
    }

    sys_locale::get_locale()
        .and_then(|tag| negotiate(&tag, available))
        .unwrap_or(default)
}

fn format_message(catalog: &Catalog, key: &str, args: Option<&FluentArgs>) -> Option<String> {
    let (id, attribute) = match key.split_once('.') {
        Some((id, attr)) => (id, Some(attr)),
        None => (key, None),
    };
    let message = catalog.bundle.get_message(id)?;
    let pattern = match attribute {
        Some(attr) => message.get_attribute(attr)?.value(),
        None => message.value()?,
    };
    let mut errors = Vec::new();
    let text = catalog.bundle.format_pattern(pattern, args, &mut errors);
    if !errors.is_empty() {
        warn_once(format!("{}: formatting {key}: {:?}", catalog.lang, errors));
    }
    Some(text.into_owned())
}

/// active language, then english, then the key itself
fn lookup_in(active: &Catalog, english: &Catalog, key: &str, args: Option<&FluentArgs>) -> String {
    if let Some(text) = format_message(active, key, args) {
        return text;
    }
    if active.lang != english.lang {
        if let Some(text) = format_message(english, key, args) {
            return text;
        }
    }
    warn_once(format!("missing key `{key}`"));
    key.to_string()
}

fn lookup(key: &str, args: Option<&FluentArgs>) -> String {
    let guard = match state().read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    lookup_in(&guard.active, &guard.english, key, args)
}

/// plain message with no variables
pub fn t(key: &str) -> String {
    lookup(key, None)
}

/// message with variables, passed as `(name, value)` pairs, e.g.
/// `&[("name", "Sam".into())]` fills `{ $name }`
pub fn t_args(key: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(name.to_string(), value.clone());
    }
    lookup(key, Some(&fluent_args))
}

/// message that pluralizes on `$count`, the ftl picks the form
pub fn t_n(key: &str, count: i64) -> String {
    t_args(key, &[("count", count.into())])
}

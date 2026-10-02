//! translation checks, run with `cargo test -p hebnix-app i18n`.
//!
//! hard failures: unparseable files, keys that don't exist in en, placeholder
//! mismatches, missing plural default, bad locales.toml, code using a key that
//! en doesn't have. missing translations only warn, set HEBNIX_I18N_STRICT=1
//! to fail on them. run `report` (ignored) for the full todo list:
//!   cargo test -p hebnix-app i18n::tests::report -- --ignored --nocapture

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fluent_syntax::ast::{
    Entry, Expression, InlineExpression, Pattern, PatternElement, Resource, Variant,
};
use fluent_syntax::parser;

use super::{DEFAULT_LANG, EMBEDDED_FTL, locale_meta};

struct Msg {
    vars: BTreeSet<String>,
    selectors: Vec<Vec<bool>>,
}

fn collect_pattern(pattern: &Pattern<&str>, vars: &mut BTreeSet<String>, sels: &mut Vec<Vec<bool>>) {
    for el in &pattern.elements {
        if let PatternElement::Placeable { expression } = el {
            collect_expression(expression, vars, sels);
        }
    }
}

fn collect_inline(expr: &InlineExpression<&str>, vars: &mut BTreeSet<String>, sels: &mut Vec<Vec<bool>>) {
    match expr {
        InlineExpression::VariableReference { id } => {
            vars.insert(id.name.to_string());
        }
        InlineExpression::Placeable { expression } => collect_expression(expression, vars, sels),
        InlineExpression::FunctionReference { arguments, .. } => {
            for arg in &arguments.positional {
                collect_inline(arg, vars, sels);
            }
            for arg in &arguments.named {
                collect_inline(&arg.value, vars, sels);
            }
        }
        _ => {}
    }
}

fn collect_expression(expr: &Expression<&str>, vars: &mut BTreeSet<String>, sels: &mut Vec<Vec<bool>>) {
    match expr {
        Expression::Inline(inline) => collect_inline(inline, vars, sels),
        Expression::Select { selector, variants } => {
            collect_inline(selector, vars, sels);
            sels.push(variants.iter().map(|v: &Variant<&str>| v.default).collect());
            for v in variants {
                collect_pattern(&v.value, vars, sels);
            }
        }
    }
}

fn parse_messages(source: &str, origin: &str, problems: &mut Vec<String>) -> BTreeMap<String, Msg> {
    let resource: Resource<&str> = match parser::parse(source) {
        Ok(r) => r,
        Err((r, errors)) => {
            for e in errors {
                problems.push(format!("{origin}: parse error {e:?}"));
            }
            r
        }
    };
    let mut out = BTreeMap::new();
    for entry in resource.body {
        if let Entry::Message(m) = entry {
            let mut vars = BTreeSet::new();
            let mut selectors = Vec::new();
            if let Some(p) = &m.value {
                collect_pattern(p, &mut vars, &mut selectors);
            }
            for attr in &m.attributes {
                collect_pattern(&attr.value, &mut vars, &mut selectors);
            }
            out.insert(m.id.name.to_string(), Msg { vars, selectors });
        }
    }
    out
}

/// every locale code -> merged messages, from the embedded files
fn load_all(problems: &mut Vec<String>) -> BTreeMap<String, BTreeMap<String, Msg>> {
    let mut all: BTreeMap<String, BTreeMap<String, Msg>> = BTreeMap::new();
    for (code, source) in EMBEDDED_FTL {
        let msgs = parse_messages(source, code, problems);
        all.entry(code.to_string()).or_default().extend(msgs);
    }
    all
}

fn rust_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                // the tests mention keys as examples
                if path.file_name().is_some_and(|n| n == "tests.rs")
                    && path.components().any(|c| c.as_os_str() == "i18n")
                {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path, text));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

/// literals passed straight to t("..."), t_args("..."), t_n("...")
fn referenced_keys(sources: &[(PathBuf, String)]) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (_, text) in sources {
        for call in ["t(\"", "t_args(\"", "t_n(\""] {
            let mut rest = text.as_str();
            while let Some(i) = rest.find(call) {
                // skip things like `format(\"` or `set(\"`
                let before = rest[..i].chars().last();
                let after = &rest[i + call.len()..];
                if !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && let Some(end) = after.find('"')
                {
                    keys.insert(after[..end].to_string());
                }
                rest = &rest[i + call.len()..];
            }
        }
    }
    keys
}

/// any string literal shaped like a key, so keys held in tables count as used
fn literal_keys(sources: &[(PathBuf, String)]) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (_, text) in sources {
        for part in text.split('"').skip(1).step_by(2) {
            if part.contains('-')
                && !part.contains(' ')
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                keys.insert(part.to_string());
            }
        }
    }
    keys
}

fn strict() -> bool {
    std::env::var("HEBNIX_I18N_STRICT").is_ok_and(|v| v == "1")
}

#[test]
fn all_locale_files_are_valid_and_consistent_with_en() {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    let all = load_all(&mut problems);
    let en = all.get(DEFAULT_LANG).expect("locales/en must exist");

    for (code, msgs) in &all {
        if code == DEFAULT_LANG {
            continue;
        }
        for (key, msg) in msgs {
            let Some(source) = en.get(key) else {
                problems.push(format!("{code}: `{key}` does not exist in en"));
                continue;
            };
            if msg.vars != source.vars {
                problems.push(format!(
                    "{code}: `{key}` placeholders {:?} differ from en {:?}",
                    msg.vars, source.vars
                ));
            }
        }
        for key in en.keys() {
            if !msgs.contains_key(key) {
                warnings.push(format!("{code}: missing `{key}` (falls back to English)"));
            }
        }
    }

    for (code, msgs) in &all {
        for (key, msg) in msgs {
            for defaults in &msg.selectors {
                if defaults.iter().filter(|d| **d).count() != 1 {
                    problems.push(format!("{code}: `{key}` selector needs exactly one *[default] variant"));
                }
            }
        }
    }

    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if strict() {
        problems.extend(warnings);
    }
    assert!(problems.is_empty(), "i18n problems:\n{}", problems.join("\n"));
}

#[test]
fn locales_toml_matches_the_folders() {
    let meta = locale_meta();
    assert!(!meta.is_empty(), "locales.toml must list at least en");
    let folders: BTreeSet<&str> = EMBEDDED_FTL.iter().map(|(c, _)| *c).collect();
    let mut problems = Vec::new();
    for m in &meta {
        if m.code.parse::<unic_langid::LanguageIdentifier>().is_err() {
            problems.push(format!("`{}` is not a valid BCP-47 tag", m.code));
        }
        if !folders.contains(m.code.as_str()) {
            problems.push(format!("`{}` is in locales.toml but has no locales/{}/*.ftl", m.code, m.code));
        }
        if m.name.trim().is_empty() {
            problems.push(format!("`{}` has no name", m.code));
        }
        if !matches!(m.status.as_str(), "reviewed" | "draft") {
            problems.push(format!("`{}` status must be reviewed or draft", m.code));
        }
    }
    for folder in folders {
        if !meta.iter().any(|m| m.code == folder) {
            problems.push(format!("locales/{folder} has no row in locales.toml"));
        }
    }
    assert!(problems.is_empty(), "locales.toml problems:\n{}", problems.join("\n"));
}

#[test]
fn every_key_used_in_code_exists_in_en() {
    let mut problems = Vec::new();
    let all = load_all(&mut problems);
    let en = all.get(DEFAULT_LANG).expect("locales/en must exist");
    let used = referenced_keys(&rust_sources());
    assert!(!used.is_empty(), "no t(\"...\") calls found, scanner broken?");
    let missing: Vec<_> = used
        .iter()
        .filter(|k| !en.contains_key(k.split('.').next().unwrap_or(k)))
        .collect();
    assert!(missing.is_empty(), "keys used in code but missing from en: {missing:?}");
}

#[test]
fn unused_en_keys_are_reported() {
    let mut problems = Vec::new();
    let all = load_all(&mut problems);
    let en = all.get(DEFAULT_LANG).expect("locales/en must exist");
    let sources = rust_sources();
    let mut seen = referenced_keys(&sources);
    seen.extend(literal_keys(&sources));
    let unused: Vec<_> = en.keys().filter(|k| !seen.contains(k.as_str())).collect();
    for k in &unused {
        eprintln!("warning: en key `{k}` is not used by any code");
    }
    if strict() {
        assert!(unused.is_empty(), "unused en keys: {unused:?}");
    }
}

#[test]
fn fallback_and_placeholders_work() {
    // these use their own catalogs so they never touch the global language,
    // other tests assert english text
    let english = super::english_catalog(None);
    let german = super::build_catalog("de", None).expect("de catalog");

    // a key present in the active language is used
    assert_eq!(
        super::lookup_in(&german, &english, "tab-console", None),
        "Konsole"
    );

    // a key only english has falls back to english, never empty or the key
    let only_en = {
        let mut bundle = fluent_bundle::concurrent::FluentBundle::new_concurrent(vec!["de".parse().unwrap()]);
        bundle.set_use_isolating(false);
        super::Catalog { lang: "de".into(), bundle }
    };
    assert_eq!(
        super::lookup_in(&only_en, &english, "tab-console", None),
        "Console"
    );

    // nothing has it: the key comes back
    assert_eq!(
        super::lookup_in(&german, &english, "this-key-does-not-exist", None),
        "this-key-does-not-exist"
    );

    let mut args = fluent_bundle::FluentArgs::new();
    args.set("value", "20");
    let text = super::lookup_in(&german, &english, "settings-set-value", Some(&args));
    assert_eq!(text, "20 setzen");
}

#[test]
fn plurals_follow_the_language_rules() {
    let english = super::english_catalog(None);
    let plural = |n: i64| {
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("count", n);
        super::lookup_in(&english, &english, "items-active-changes", Some(&args))
    };
    assert_eq!(plural(1), "1 active change");
    assert_eq!(plural(2), "2 active changes");
    assert_eq!(plural(0), "0 active changes");
}

#[test]
fn language_negotiation_picks_sensible_matches() {
    let avail = vec!["en".to_string(), "de".to_string(), "zh-Hant".to_string()];
    assert_eq!(super::negotiate("de-DE", &avail).as_deref(), Some("de"));
    assert_eq!(super::negotiate("de_AT.UTF-8", &avail).as_deref(), Some("de"));
    assert_eq!(super::negotiate("EN-us", &avail).as_deref(), Some("en"));
    assert_eq!(super::negotiate("zh-Hant-TW", &avail).as_deref(), Some("zh-Hant"));
    assert_eq!(super::negotiate("xx", &avail), None);
}

#[test]
fn number_formatting_uses_language_separators() {
    assert_eq!(super::format::int_for("en", 1234567), "1,234,567");
    assert_eq!(super::format::number_for("en", 1234.5, 1), "1,234.5");
    assert_eq!(super::format::int_for("de-DE", 1234567), "1.234.567");
    assert_eq!(super::format::number_for("de", 1234.5, 1), "1.234,5");
    assert_eq!(super::format::int_for("en", -1000), "-1,000");
}

/// translator todo list: `cargo test -p hebnix-app i18n::tests::report -- --ignored --nocapture`
#[test]
#[ignore]
fn report() {
    let mut problems = Vec::new();
    let all = load_all(&mut problems);
    let en = all.get(DEFAULT_LANG).expect("locales/en must exist");
    println!("en: {} messages", en.len());
    for (code, msgs) in &all {
        if code == DEFAULT_LANG {
            continue;
        }
        let missing: Vec<_> = en.keys().filter(|k| !msgs.contains_key(*k)).collect();
        let orphans: Vec<_> = msgs.keys().filter(|k| !en.contains_key(*k)).collect();
        println!(
            "{code}: {}/{} translated, {} missing, {} not in en",
            msgs.len() - orphans.len(),
            en.len(),
            missing.len(),
            orphans.len()
        );
        for k in missing {
            println!("  missing: {k}");
        }
        for k in orphans {
            println!("  orphan:  {k}");
        }
    }
    for p in problems {
        println!("problem: {p}");
    }
}

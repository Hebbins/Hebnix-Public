//! number and size formatting. kept out of the .ftl files on purpose: these
//! depend on the language's separators, not on translated wording.

fn separators(lang: &str) -> (&'static str, &'static str) {
    // (group, decimal)
    let primary = lang.split('-').next().unwrap_or(lang);
    match primary {
        "de" | "es" | "it" | "nl" | "pt" | "id" | "tr" | "da" | "el" | "ro" | "hr" | "sl" => {
            (".", ",")
        }
        "fr" | "ru" | "uk" | "pl" | "cs" | "sk" | "sv" | "fi" | "nb" | "no" | "bg" | "hu"
        | "lt" | "lv" | "et" => ("\u{a0}", ","),
        _ => (",", "."),
    }
}

fn group_digits(digits: &str, sep: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 * sep.len());
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

pub fn int(n: i64) -> String {
    int_for(&super::current(), n)
}

pub fn number(n: f64, decimals: usize) -> String {
    number_for(&super::current(), n, decimals)
}

pub fn int_for(lang: &str, n: i64) -> String {
    let (group, _) = separators(lang);
    let sign = if n < 0 { "-" } else { "" };
    format!(
        "{sign}{}",
        group_digits(&n.unsigned_abs().to_string(), group)
    )
}

pub fn number_for(lang: &str, n: f64, decimals: usize) -> String {
    let (group, decimal) = separators(lang);
    let raw = format!("{:.*}", decimals, n.abs());
    let (whole, frac) = match raw.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (raw.as_str(), None),
    };
    let sign = if n < 0.0 && raw.chars().any(|c| c != '0' && c != '.') {
        "-"
    } else {
        ""
    };
    match frac {
        Some(f) => format!("{sign}{}{decimal}{f}", group_digits(whole, group)),
        None => format!("{sign}{}", group_digits(whole, group)),
    }
}

/// "1.5 MB" with the language's decimal mark. unit symbols stay as is.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{} {}", number(value, 1), UNITS[unit])
    }
}

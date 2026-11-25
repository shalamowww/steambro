//! The app's texts in the user's language. One file per language in `ui/i18n/` is the only place
//! the texts live: the window reads them through the `strings` command, and the Rust side reads
//! the few it shows itself (the tray menu, notifications, error boxes) through `text`. The language
//! is the user's choice in the settings, else Windows' display language, else English. A text
//! missing from a language falls back to English.

use std::sync::OnceLock;

use serde_json::Value;

/// The languages steambro has, as `(code, name in that language)`. English comes first and is
/// the fallback.
pub const LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("ru", "Русский"), ("es", "Español"), ("pt-BR", "Português (Brasil)")];

const FILES: &[(&str, &str)] = &[
    ("en", include_str!("../ui/i18n/en.json")),
    ("ru", include_str!("../ui/i18n/ru.json")),
    ("es", include_str!("../ui/i18n/es.json")),
    ("pt-BR", include_str!("../ui/i18n/pt-BR.json")),
];

/// The texts of a language, parsed once.
pub fn strings(code: &str) -> &'static Value {
    static PARSED: OnceLock<Vec<(&'static str, Value)>> = OnceLock::new();
    let all = PARSED.get_or_init(|| FILES.iter().map(|(code, json)| (*code, serde_json::from_str(json).unwrap_or_else(|e| panic!("ui/i18n/{code}.json: {e}")))).collect());
    all.iter().find(|(c, _)| *c == code).or_else(|| all.first()).map(|(_, v)| v).expect("English is built in")
}

/// Windows' display language (the language of its own menus, not the regional format) as one of
/// `LANGUAGES`, else English.
pub fn system_language() -> &'static str {
    use windows::Win32::Globalization::GetUserDefaultUILanguage;
    // The primary language of the LANGID: en 0x09, es 0x0a, pt 0x16, ru 0x19 (winnt.h). Every
    // Portuguese gets the Brazilian texts: they are the only ones steambro has.
    let primary = unsafe { GetUserDefaultUILanguage() } & 0x3ff;
    let code = match primary {
        0x09 => "en",
        0x0a => "es",
        0x16 => "pt-BR",
        0x19 => "ru",
        _ => "en",
    };
    LANGUAGES.iter().map(|(code, _)| *code).find(|c| *c == code).unwrap_or("en")
}

/// The language in use: the user's choice when it is one steambro has, else Windows'.
pub fn language() -> &'static str {
    let chosen = crate::settings::Settings::load().language;
    chosen.and_then(|c| LANGUAGES.iter().map(|(code, _)| *code).find(|code| *code == c)).unwrap_or_else(system_language)
}

/// A text by its dotted key, like `tray.open`, with `{name}` placeholders filled from `vars`; in
/// English when the language lacks it; the key itself when English lacks it too.
pub fn text(key: &str, vars: &[(&str, &str)]) -> String {
    let lang = language();
    let found = lookup(strings(lang), key).or_else(|| lookup(strings("en"), key)).unwrap_or(key);
    vars.iter().fold(found.to_string(), |s, (name, value)| s.replace(&format!("{{{name}}}"), value))
}

/// Like `text`, for a key whose value is a plural form (`one`, `few`, `many`, `other`): the form
/// for `count`, with `{count}` filled in too.
pub fn plural(key: &str, count: usize, vars: &[(&str, &str)]) -> String {
    let lang = language();
    let form = plural_form(lang, count);
    let pick = |value: &'static Value| value.get(form).or_else(|| value.get("other")).and_then(Value::as_str);
    let found = lookup_value(strings(lang), key).and_then(pick).or_else(|| lookup_value(strings("en"), key).and_then(|v| v.get(plural_form("en", count)).or_else(|| v.get("other")).and_then(Value::as_str))).unwrap_or(key);
    let count_text = count.to_string();
    let mut s = found.replace("{count}", &count_text);
    for (name, value) in vars {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

/// The CLDR plural category of `count` in `lang`, as the JSON files key their forms.
fn plural_form(lang: &str, count: usize) -> &'static str {
    match lang {
        "ru" | "uk" => match (count % 10, count % 100) {
            (1, n) if n != 11 => "one",
            (2..=4, n) if !(12..=14).contains(&n) => "few",
            _ => "many",
        },
        // Brazilian Portuguese counts 0 and 1 as "one".
        "pt-BR" => {
            if count <= 1 {
                "one"
            } else {
                "other"
            }
        }
        _ => {
            if count == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

fn lookup_value<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(root, |v, part| v.get(part))
}

fn lookup<'a>(root: &'a Value, key: &str) -> Option<&'a str> {
    lookup_value(root, key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_file_parses_and_has_the_tray() {
        for (code, _) in LANGUAGES {
            assert!(lookup(strings(code), "tray.open").is_some(), "{code} lacks tray.open");
        }
    }

    #[test]
    fn the_russian_plural_forms_follow_the_count() {
        assert_eq!(plural_form("ru", 1), "one");
        assert_eq!(plural_form("ru", 21), "one");
        assert_eq!(plural_form("ru", 3), "few");
        assert_eq!(plural_form("ru", 11), "many");
        assert_eq!(plural_form("ru", 5), "many");
        assert_eq!(plural_form("en", 1), "one");
        assert_eq!(plural_form("es", 2), "other");
        assert_eq!(plural_form("pt-BR", 0), "one");
        assert_eq!(plural_form("pt-BR", 2), "other");
    }

    #[test]
    fn placeholders_are_filled() {
        let en = strings("en");
        assert!(lookup(en, "notice.one_installed").unwrap().contains("{name}"));
        let s = text("notice.one_installed", &[("name", "Fortnite")]);
        assert!(s.contains("Fortnite") && !s.contains("{name}"), "{s}");
    }
}

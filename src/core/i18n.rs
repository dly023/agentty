use std::collections::HashMap;
use std::sync::OnceLock;

pub use agentty_core::core::config::LocalePreference;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Locale {
    ZhCn,
    #[default]
    EnUs,
}

pub trait ResolveLocale {
    fn resolve(self) -> Locale;
}

impl ResolveLocale for LocalePreference {
    fn resolve(self) -> Locale {
        match self {
            Self::ZhCn => Locale::ZhCn,
            Self::EnUs => Locale::EnUs,
            Self::System => system_locale(),
        }
    }
}

fn system_locale() -> Locale {
    // GUI apps launched from Finder do not inherit shell locale variables,
    // so the OS preference is the fallback authority. Resolved once.
    static CACHED: OnceLock<Locale> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let env = std::env::var("LC_ALL")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|v| !v.is_empty()))
            .or_else(|| std::env::var("LANG").ok())
            .unwrap_or_default();
        if let Some(locale) = language_from_env(&env) {
            return locale;
        }
        if macos_prefers_chinese() {
            Locale::ZhCn
        } else {
            Locale::EnUs
        }
    })
}

/// Map a libc locale name to a UI language. Neutral POSIX/C locales are not
/// language preferences (I18N-SYSTEM-C-LOCALE-05).
fn language_from_env(env: &str) -> Option<Locale> {
    let env = env.trim().to_ascii_lowercase();
    if env.is_empty() || is_neutral_posix_locale(&env) {
        return None;
    }
    if env.starts_with("zh") {
        Some(Locale::ZhCn)
    } else {
        Some(Locale::EnUs)
    }
}

fn is_neutral_posix_locale(env: &str) -> bool {
    matches!(env, "c" | "posix")
        || env.starts_with("c.")
        || env.starts_with("c_")
        || env.starts_with("posix.")
}

#[cfg(target_os = "macos")]
fn macos_prefers_chinese() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .map(|out| {
            out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .to_ascii_lowercase()
                    .contains("zh")
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn macos_prefers_chinese() -> bool {
    false
}

/// Validate a line-oriented catalog blob before it can ship.
/// Empty values and duplicate keys are hard failures (I18N-EXHAUSTIVE-TRANSLATION-06).
pub fn catalog_integrity_errors(input: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut seen = HashMap::new();
    for (lineno, raw) in input.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            errors.push(format!("line {}: missing ':' separator", lineno + 1));
            continue;
        };
        let key = key.trim();
        let mut value = value.trim();
        if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            value = inner;
        }
        if key.is_empty() {
            errors.push(format!("line {}: empty key", lineno + 1));
            continue;
        }
        if let Some(first) = seen.insert(key.to_string(), lineno + 1) {
            errors.push(format!(
                "line {}: duplicate key '{key}' (first at line {first})",
                lineno + 1
            ));
            continue;
        }
        if value.trim().is_empty() {
            errors.push(format!("line {}: empty value for '{key}'", lineno + 1));
        }
    }
    errors
}

fn parse_catalog(input: &'static str) -> HashMap<&'static str, &'static str> {
    let integrity = catalog_integrity_errors(input);
    debug_assert!(
        integrity.is_empty(),
        "catalog integrity failed: {integrity:?}"
    );
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            let mut value = value.trim();
            // YAML double-quotes protect colons / leading braces; strip them here.
            if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                value = inner;
            }
            if value.trim().is_empty() {
                return None;
            }
            // Catalog files are line-based; multi-line messages escape newlines.
            let value: &'static str = if value.contains("\\n") || value.contains("\\\"") {
                Box::leak(
                    value
                        .replace("\\n", "\n")
                        .replace("\\\"", "\"")
                        .into_boxed_str(),
                )
            } else {
                value
            };
            Some((key.trim(), value))
        })
        .collect()
}

fn catalog(locale: Locale) -> &'static HashMap<&'static str, &'static str> {
    static ZH: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    static EN: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    match locale {
        Locale::ZhCn => {
            ZH.get_or_init(|| parse_catalog(include_str!("../../assets/i18n/zh-CN.yaml")))
        }
        Locale::EnUs => {
            EN.get_or_init(|| parse_catalog(include_str!("../../assets/i18n/en-US.yaml")))
        }
    }
}

pub fn tr(locale: Locale, key: &'static str) -> &'static str {
    if let Some(value) = catalog(locale).get(key).copied() {
        return value;
    }
    // Cross-locale fallback is a last resort for unknown keys. Shipped catalogs
    // must already be exhaustive (I18N-EXHAUSTIVE-TRANSLATION-06); this path is
    // for typos / future keys, not intentional blank translations.
    debug_assert!(
        locale == Locale::EnUs || catalog(Locale::EnUs).get(key).is_none(),
        "preferred locale {locale:?} missing catalog key '{key}' that exists in en-US"
    );
    catalog(Locale::EnUs).get(key).copied().unwrap_or(key)
}

pub fn current(cx: &gpui::App, key: &'static str) -> &'static str {
    tr(
        cx.global::<crate::core::config::Config>().locale.resolve(),
        key,
    )
}

/// Translate a short option label (segmented/radio choices) through the
/// `opt.<slug>` catalog namespace. Untranslated or non-verbal values
/// (numbers, key names, host names) fall back to the literal unchanged.
pub fn tr_opt(locale: Locale, label: &'static str) -> &'static str {
    let slug: String = label
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if slug.is_empty() || slug.chars().all(|c| c.is_ascii_digit()) {
        return label;
    }
    let key = format!("opt.{slug}");
    catalog(locale)
        .get(key.as_str())
        .copied()
        .or_else(|| catalog(Locale::EnUs).get(key.as_str()).copied())
        .unwrap_or(label)
}

pub fn current_opt(cx: &gpui::App, label: &'static str) -> &'static str {
    tr_opt(
        cx.global::<crate::core::config::Config>().locale.resolve(),
        label,
    )
}

/// Format a catalog template with `{name}` placeholders.
/// Unknown placeholders are left intact so templates stay debuggable.
pub fn trf(locale: Locale, key: &'static str, args: &[(&str, &str)]) -> String {
    let mut out = tr(locale, key).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

pub fn current_format(cx: &gpui::App, key: &'static str, args: &[(&str, &str)]) -> String {
    trf(
        cx.global::<crate::core::config::Config>().locale.resolve(),
        key,
        args,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_have_identical_keys() {
        let mut zh: Vec<_> = catalog(Locale::ZhCn).keys().copied().collect();
        let mut en: Vec<_> = catalog(Locale::EnUs).keys().copied().collect();
        zh.sort_unstable();
        en.sort_unstable();
        assert_eq!(zh, en);
    }

    #[test]
    fn catalog_entries_are_non_empty() {
        for locale in [Locale::ZhCn, Locale::EnUs] {
            for (key, value) in catalog(locale) {
                assert!(
                    !value.trim().is_empty(),
                    "{locale:?} key '{key}' must not be blank"
                );
            }
        }
    }

    #[test]
    fn preferred_locale_never_needs_cross_locale_fallback() {
        let en = catalog(Locale::EnUs);
        let zh = catalog(Locale::ZhCn);
        for key in en.keys() {
            assert!(
                zh.contains_key(key),
                "zh-CN must define '{key}' without borrowing en-US at lookup time"
            );
        }
        for key in zh.keys() {
            assert!(
                en.contains_key(key),
                "en-US must define '{key}' so locales stay exhaustive peers"
            );
        }
    }

    #[test]
    fn catalog_integrity_rejects_empty_and_duplicate_keys() {
        let empty = catalog_integrity_errors("ok.key: value\nbad.key:\n");
        assert!(empty.iter().any(|e| e.contains("empty value")), "{empty:?}");
        let dup = catalog_integrity_errors("same.key: one\nsame.key: two\n");
        assert!(dup.iter().any(|e| e.contains("duplicate key")), "{dup:?}");
        assert!(catalog_integrity_errors("good.key: hi\n").is_empty());
        assert!(catalog_integrity_errors(include_str!("../../assets/i18n/en-US.yaml")).is_empty());
        assert!(catalog_integrity_errors(include_str!("../../assets/i18n/zh-CN.yaml")).is_empty());
    }

    #[test]
    fn format_replaces_named_placeholders() {
        let rendered = trf(Locale::EnUs, "home.reopen_tab", &[("name", "alpha")]);
        assert_eq!(rendered, "Reopen “alpha”");
        let rendered_zh = trf(Locale::ZhCn, "home.reopen_tab", &[("name", "alpha")]);
        assert_eq!(rendered_zh, "重新打开 “alpha”");
        // Unknown placeholders stay visible instead of panicking.
        let untouched = trf(Locale::EnUs, "home.reopen_tab", &[]);
        assert_eq!(untouched, "Reopen “{name}”");
    }

    #[test]
    fn escaped_newlines_render_as_real_newlines() {
        let detail = tr(Locale::ZhCn, "mismatch.detail");
        assert!(
            detail.contains('\n'),
            "multi-line messages keep real newlines"
        );
        assert!(!detail.contains("\\n"), "no literal escape survives");
    }

    #[test]
    fn option_labels_translate_and_fall_back() {
        assert_eq!(tr_opt(Locale::ZhCn, "Off"), "关闭");
        assert_eq!(tr_opt(Locale::EnUs, "Off"), "Off");
        assert_eq!(tr_opt(Locale::ZhCn, "2FA"), "两步验证");
        // Non-verbal values pass through untouched.
        assert_eq!(tr_opt(Locale::ZhCn, "8080"), "8080");
        assert_eq!(tr_opt(Locale::ZhCn, "tmux"), "tmux");
        assert_eq!(tr_opt(Locale::ZhCn, "Ctrl-A"), "Ctrl-A");
    }

    #[test]
    fn locale_override_is_deterministic() {
        assert_eq!(
            tr(LocalePreference::ZhCn.resolve(), "session.resume"),
            "恢复"
        );
        assert_eq!(
            tr(LocalePreference::EnUs.resolve(), "session.resume"),
            "Resume"
        );
    }

    #[test]
    fn neutral_posix_lang_is_not_a_language_preference() {
        assert_eq!(language_from_env(""), None);
        assert_eq!(language_from_env("C"), None);
        assert_eq!(language_from_env("POSIX"), None);
        assert_eq!(language_from_env("C.UTF-8"), None);
        assert_eq!(language_from_env("c.utf8"), None);
        assert_eq!(language_from_env("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(language_from_env("en_US.UTF-8"), Some(Locale::EnUs));
    }
}

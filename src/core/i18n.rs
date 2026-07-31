use std::collections::HashMap;
use std::sync::OnceLock;

pub use agentty_core::core::config::LocalePreference;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    ZhCn,
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
    let value = std::env::var("LC_ALL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|v| !v.is_empty()))
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if value.starts_with("zh") {
        Locale::ZhCn
    } else {
        Locale::EnUs
    }
}

fn parse_catalog(input: &'static str) -> HashMap<&'static str, &'static str> {
    input
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            // Catalog files are line-based; multi-line messages escape newlines.
            let value: &'static str = if value.contains("\\n") {
                Box::leak(value.replace("\\n", "\n").into_boxed_str())
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
    catalog(locale)
        .get(key)
        .copied()
        .or_else(|| catalog(Locale::EnUs).get(key).copied())
        .unwrap_or(key)
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
}

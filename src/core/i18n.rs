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
            Some((key.trim(), value.trim()))
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

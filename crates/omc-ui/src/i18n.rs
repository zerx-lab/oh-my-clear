//! UI language. Strings live in `locales/app.yml` (rust-i18n, `t!("key")`); the app supports
//! English and Simplified Chinese and follows the OS by default. gpui-component's own
//! strings (dialogs, date pickers, settings search) switch with the same locale.
//!
//! The locale is process-global state outside GPUI's change tracking: changing it goes
//! through [`crate::settings::UiSettings::update`], which refreshes every window and
//! rebuilds the native menus.

use gpui_kit::SharedString;

/// English locale code.
const EN: &str = "en";
/// Simplified Chinese locale code.
const ZH_CN: &str = "zh-CN";

/// Language selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    /// Follow the OS preferred language.
    #[default]
    System,
    /// English.
    English,
    /// Simplified Chinese.
    SimplifiedChinese,
}

impl Language {
    /// Every choice, in menu order.
    pub const ALL: [Self; 3] = [Self::System, Self::English, Self::SimplifiedChinese];

    /// Stable identifier.
    pub const fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => EN,
            Self::SimplifiedChinese => ZH_CN,
        }
    }

    /// Inverse of [`Self::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == key)
    }

    /// The locale this choice resolves to now.
    pub fn locale(self) -> &'static str {
        match self {
            Self::System => resolve_locale(sys_locale::get_locale().as_deref()),
            Self::English => EN,
            Self::SimplifiedChinese => ZH_CN,
        }
    }

    /// Menu label. Languages are named in their own script so a user who cannot read the
    /// current UI language still finds theirs.
    pub fn label(self) -> SharedString {
        match self {
            Self::System => rust_i18n::t!("language.system").to_string().into(),
            Self::English => "English".into(),
            Self::SimplifiedChinese => "简体中文".into(),
        }
    }
}

/// Maps an OS language tag (`zh-Hans-CN`, `zh_CN.UTF-8`, `en-US`, …) to a supported
/// locale. Every Chinese variant maps to Simplified Chinese, the only Chinese translation;
/// anything else, or an unknown tag, is English.
pub(crate) fn resolve_locale(tag: Option<&str>) -> &'static str {
    let Some(tag) = tag else {
        return EN;
    };
    let primary = tag
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if primary == "zh" { ZH_CN } else { EN }
}

/// Makes `language` the active locale for the app and gpui-component strings.
pub(crate) fn apply(language: Language) {
    let locale = language.locale();
    gpui_kit::component::set_locale(locale);
    tracing::debug!(?language, locale, "UI language applied");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_tags_resolve_to_supported_locales() {
        for (tag, expected) in [
            (Some("zh-Hans-CN"), ZH_CN),
            (Some("zh_CN.UTF-8"), ZH_CN),
            (Some("zh-TW"), ZH_CN),
            (Some("ZH"), ZH_CN),
            (Some("en-US"), EN),
            (Some("de_DE@euro"), EN),
            (Some("C"), EN),
            (Some(""), EN),
            (None, EN),
        ] {
            assert_eq!(resolve_locale(tag), expected, "tag {tag:?}");
        }
    }

    #[test]
    fn english_and_chinese_define_the_same_keys() {
        let keys = |locale: &str| -> std::collections::BTreeSet<String> {
            crate::_rust_i18n_backend()
                .messages_for_locale(locale)
                .unwrap_or_default()
                .into_iter()
                .map(|(key, _)| key.into_owned())
                .collect()
        };
        let (en, zh) = (keys(EN), keys(ZH_CN));
        assert!(!en.is_empty(), "locales/app.yml must load");
        let only_en: Vec<_> = en.difference(&zh).collect();
        let only_zh: Vec<_> = zh.difference(&en).collect();
        assert!(
            only_en.is_empty() && only_zh.is_empty(),
            "untranslated keys: en-only {only_en:?}, zh-CN-only {only_zh:?}"
        );
    }
}

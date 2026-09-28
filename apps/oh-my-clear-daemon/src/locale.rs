//! Tray language: strings live in `locales/app.yml` (rust-i18n, `t!("key")`), English and
//! Simplified Chinese, following the OS like the UI's default (`omc_ui::i18n`).

/// English locale code.
const EN: &str = "en";
/// Simplified Chinese locale code.
const ZH_CN: &str = "zh-CN";

/// Makes the OS language the tray's locale. Call before the tray is built.
pub(crate) fn apply() {
    let locale = resolve(sys_locale::get_locale().as_deref());
    rust_i18n::set_locale(locale);
    tracing::debug!(locale, "tray language applied");
}

/// Maps an OS language tag (`zh-Hans-CN`, `zh_CN.UTF-8`, `en-US`, …) to a supported
/// locale: every Chinese variant is Simplified Chinese, anything else English (the same
/// mapping as the UI).
fn resolve(tag: Option<&str>) -> &'static str {
    let primary = tag
        .and_then(|tag| tag.split(['-', '_', '.', '@']).next())
        .unwrap_or_default();
    if primary.eq_ignore_ascii_case("zh") {
        ZH_CN
    } else {
        EN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_tags_resolve_to_supported_locales() {
        for (tag, expected) in [
            (Some("zh-Hans-CN"), ZH_CN),
            (Some("zh_CN.UTF-8"), ZH_CN),
            (Some("ZH"), ZH_CN),
            (Some("en-US"), EN),
            (Some("C"), EN),
            (None, EN),
        ] {
            assert_eq!(resolve(tag), expected, "tag {tag:?}");
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
        assert_eq!(en, zh, "every tray string is translated");
    }
}

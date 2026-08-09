use serde::{Deserialize, Serialize};

/// The language preference Waku persists. `System` resolves to one of the two
/// locales Waku deliberately ships today.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppLanguage {
    System,
    English,
    SimplifiedChinese,
}

impl AppLanguage {
    pub const ALL: [Self; 3] = [Self::System, Self::English, Self::SimplifiedChinese];

    pub fn locale(self) -> &'static str {
        match self.resolved() {
            Self::System => unreachable!("system language always resolves to a shipped locale"),
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    /// Explicit language names are autonyms so the selector remains
    /// understandable even when the current locale is unfamiliar.
    pub fn label(self) -> String {
        match self {
            Self::System => translate("language.system"),
            Self::English => "English".to_owned(),
            Self::SimplifiedChinese => "简体中文".to_owned(),
        }
    }

    pub fn resolved(self) -> Self {
        match self {
            Self::System => Self::from_system(),
            explicit => explicit,
        }
    }

    fn from_system() -> Self {
        Self::from_locale_id(&system_locale())
    }

    fn from_locale_id(locale: &str) -> Self {
        let locale = locale.replace('_', "-").to_ascii_lowercase();
        if locale == "zh-cn" || locale == "zh-sg" || locale.starts_with("zh-hans") {
            Self::SimplifiedChinese
        } else {
            Self::English
        }
    }
}

impl Default for AppLanguage {
    fn default() -> Self {
        Self::System
    }
}

pub fn set_language(language: AppLanguage) {
    rust_i18n::set_locale(language.locale());
}

pub fn translate(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

pub fn is_simplified_chinese() -> bool {
    &*rust_i18n::locale() == "zh-CN"
}

#[cfg(target_os = "macos")]
fn system_locale() -> String {
    use objc2_foundation::NSLocale;

    NSLocale::preferredLanguages()
        .firstObject()
        .map(|locale| locale.to_string())
        .unwrap_or_else(|| "en".to_owned())
}

#[cfg(not(target_os = "macos"))]
fn system_locale() -> String {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "en".to_owned())
        .split('.')
        .next()
        .unwrap_or("en")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_locale_ids_are_supported() {
        assert_eq!(AppLanguage::English.locale(), "en");
        assert_eq!(AppLanguage::SimplifiedChinese.locale(), "zh-CN");
        let locales = rust_i18n::available_locales!();
        assert_eq!(locales.len(), 2);
        assert!(locales.iter().any(|locale| locale.as_ref() == "en"));
        assert!(locales.iter().any(|locale| locale.as_ref() == "zh-CN"));
    }

    #[test]
    fn language_names_are_autonyms() {
        assert_eq!(AppLanguage::English.label(), "English");
        assert_eq!(AppLanguage::SimplifiedChinese.label(), "简体中文");
    }

    #[test]
    fn system_is_the_default_persisted_preference_and_resolves_to_a_shipped_locale() {
        assert_eq!(AppLanguage::default(), AppLanguage::System);
        assert_eq!(
            serde_json::to_string(&AppLanguage::System).unwrap(),
            r#""system""#
        );
        assert!(matches!(AppLanguage::System.locale(), "en" | "zh-CN"));
    }

    #[test]
    fn simplified_chinese_system_locales_are_detected_without_enabling_traditional_chinese() {
        assert_eq!(
            AppLanguage::from_locale_id("zh-Hans-CN"),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(
            AppLanguage::from_locale_id("zh_SG"),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(
            AppLanguage::from_locale_id("zh-Hant-TW"),
            AppLanguage::English
        );
    }

    #[test]
    fn translations_are_complete_and_interpolate_naturally() {
        assert_eq!(
            &*rust_i18n::t!("settings.general", locale = "zh-CN"),
            "通用"
        );
        assert_eq!(
            &*rust_i18n::t!(
                "computer_use.allow_control",
                locale = "zh-CN",
                app = "Finder"
            ),
            "允许 Waku 控制“Finder”吗？"
        );
        assert_eq!(
            &*rust_i18n::t!("session.rewound", locale = "zh-CN", turn = 3),
            "已回退到第 3 轮任务之前。"
        );
    }

    #[test]
    fn task_creation_copy_uses_task_terminology() {
        assert_eq!(&*rust_i18n::t!("menu.new_task", locale = "en"), "New Task");
        assert_eq!(
            &*rust_i18n::t!("command_palette.new_task", locale = "en"),
            "New task"
        );
        assert_eq!(
            &*rust_i18n::t!("providers.disabled_for_new_tasks", locale = "en"),
            "Disabled for new tasks"
        );
    }
}

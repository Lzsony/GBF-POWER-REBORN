use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CachePreferences {
    pub prefetch_enabled: bool,
    pub warmup_enabled: bool,
}
impl Default for CachePreferences {
    fn default() -> Self {
        Self {
            prefetch_enabled: true,
            warmup_enabled: true,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CachePreferencePatch {
    pub prefetch_enabled: Option<bool>,
    pub warmup_enabled: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    #[default]
    Auto,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub enum Language {
    #[serde(rename = "zh-CN")]
    Simplified,
    #[default]
    #[serde(rename = "zh-TW")]
    Traditional,
}
impl Language {
    pub fn from_system(value: &str) -> Self {
        let value = value.to_ascii_lowercase().replace('_', "-");
        if !value.starts_with("zh") {
            return Self::Traditional;
        }
        if value.contains("hant") {
            return Self::Traditional;
        }
        if value.contains("hans") {
            return Self::Simplified;
        }
        if value.split('-').any(|v| matches!(v, "tw" | "hk" | "mo")) {
            Self::Traditional
        } else {
            Self::Simplified
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub theme: Theme,
    pub language: Language,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locale_script_precedence_and_fallback() {
        for (locale, language) in [
            ("zh-Hans-HK", Language::Simplified),
            ("zh-Hant-CN", Language::Traditional),
            ("zh_TW", Language::Traditional),
            ("zh-SG", Language::Simplified),
            ("en-GB", Language::Traditional),
            ("ja-JP", Language::Traditional),
            ("ja_JP", Language::Traditional),
            ("ja", Language::Traditional),
            ("fr-FR", Language::Traditional),
        ] {
            assert_eq!(Language::from_system(locale), language);
        }
        assert_eq!(Preferences::default().language, Language::Traditional);
        let preferences = Preferences {
            language: Language::Traditional,
            ..Default::default()
        };
        let json = serde_json::to_string(&preferences).unwrap();
        assert_eq!(
            serde_json::from_str::<Preferences>(&json).unwrap(),
            preferences
        );
        assert!(json.contains("\"zh-TW\""));
        assert!(
            serde_json::from_str::<Preferences>(r#"{"theme":"rainbow","language":"en"}"#).is_err()
        );
    }
}

//! Eight shipped UI languages. Real OS names, paths and errors are never translated.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    sync::{Once, OnceLock},
};
use windows::{
    Win32::Globalization::{
        GetUserDefaultLocaleName, GetUserDefaultUILanguage, GetUserPreferredUILanguages,
        LCIDToLocaleName, MUI_LANGUAGE_NAME,
    },
    core::PWSTR,
};

/// Saved preference. `System` resolves to one of the eight concrete languages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    System,
    ZhCn,
    EnUs,
    JaJp,
    KoKr,
    DeDe,
    FrFr,
    EsEs,
    PtBr,
}
impl Language {
    pub const SUPPORTED: [Self; 8] = [
        Self::ZhCn,
        Self::EnUs,
        Self::JaJp,
        Self::KoKr,
        Self::DeDe,
        Self::FrFr,
        Self::EsEs,
        Self::PtBr,
    ];
    pub const ALL: [Self; 9] = [
        Self::System,
        Self::ZhCn,
        Self::EnUs,
        Self::JaJp,
        Self::KoKr,
        Self::DeDe,
        Self::FrFr,
        Self::EsEs,
        Self::PtBr,
    ];
    pub const fn tag(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
            Self::JaJp => "ja-JP",
            Self::KoKr => "ko-KR",
            Self::DeDe => "de-DE",
            Self::FrFr => "fr-FR",
            Self::EsEs => "es-ES",
            Self::PtBr => "pt-BR",
        }
    }
    /// Native language names stay recognizable regardless of the current UI.
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::System => "System default",
            Self::ZhCn => "简体中文",
            Self::EnUs => "English",
            Self::JaJp => "日本語",
            Self::KoKr => "한국어",
            Self::DeDe => "Deutsch",
            Self::FrFr => "Français",
            Self::EsEs => "Español",
            Self::PtBr => "Português (Brasil)",
        }
    }
    /// Map a supported language family to its shipped regional catalog.
    /// Only Simplified Chinese and Brazilian Portuguese catalogs ship today.
    pub fn from_locale(locale: &str) -> Option<Self> {
        let normalized = locale.trim().replace('_', "-").to_ascii_lowercase();
        match normalized.split('-').next()? {
            "zh" => Some(Self::ZhCn),
            "en" => Some(Self::EnUs),
            "ja" => Some(Self::JaJp),
            "ko" => Some(Self::KoKr),
            "de" => Some(Self::DeDe),
            "fr" => Some(Self::FrFr),
            "es" => Some(Self::EsEs),
            "pt" => Some(Self::PtBr),
            _ => None,
        }
    }
    pub fn from_preference(value: &str) -> Self {
        Self::from_locale(value).unwrap_or(Self::System)
    }
    pub fn resolve(self) -> Self {
        if self == Self::System {
            detect_system_language().language
        } else {
            self
        }
    }
}
impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.tag())
    }
}
impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // Settings are JSON. Consume unsupported field values locally, so a
        // future/invalid language cannot reset refresh/theme/column settings.
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value
            .as_str()
            .map(Self::from_preference)
            .unwrap_or_default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemLanguageSource {
    PreferredUiLanguages,
    UserUiLanguage,
    UserLocale,
    EnglishFallback,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemLanguage {
    pub language: Language,
    /// Original Windows tag, without inventing a locale when detection fails.
    pub requested_locale: Option<String>,
    pub source: SystemLanguageSource,
}

/// Bounded parser for the UTF-16 MULTI_SZ returned by Windows.
fn parse_language_list(buffer: &[u16], reported_count: u32) -> Option<Vec<String>> {
    if reported_count == 0 || reported_count > 128 {
        return None;
    }
    let mut tags = Vec::new();
    let mut offset = 0;
    for _ in 0..reported_count {
        let tail = buffer.get(offset..)?;
        let length = tail.iter().position(|value| *value == 0)?;
        if length == 0 {
            return None;
        }
        tags.push(String::from_utf16(&tail[..length]).ok()?);
        offset = offset.checked_add(length + 1)?;
    }
    // Every string's NUL plus the final MULTI_SZ NUL must be present.
    (buffer.get(offset) == Some(&0)).then_some(tags)
}
fn preferred_ui_languages() -> Option<Vec<String>> {
    let (mut count, mut length) = (0_u32, 0_u32);
    // SAFETY: first call has no output buffer and only asks for bounded size.
    unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length) }
        .ok()?;
    if !(2..=32_768).contains(&length) {
        return None;
    }
    let mut buffer = vec![0_u16; length as usize];
    // SAFETY: UTF-16 output has exactly the required capacity; no OS mutation.
    unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    }
    .ok()?;
    if length as usize > buffer.len() {
        return None;
    }
    parse_language_list(&buffer[..length as usize], count)
}
fn locale_name(read: impl FnOnce(&mut [u16]) -> i32) -> Option<String> {
    let mut buffer = [0_u16; 85]; // Windows LOCALE_NAME_MAX_LENGTH, including NUL.
    let length = usize::try_from(read(&mut buffer)).ok()?;
    if length < 2 || length > buffer.len() || buffer[length - 1] != 0 {
        return None;
    }
    String::from_utf16(&buffer[..length - 1]).ok()
}
/// Read-only startup/selection query. UI language and numeric region are distinct.
pub fn detect_system_language() -> SystemLanguage {
    let preferred = preferred_ui_languages().unwrap_or_default();
    let first = preferred.first().cloned();
    if let Some((tag, language)) = preferred
        .iter()
        .find_map(|tag| Language::from_locale(tag).map(|language| (tag, language)))
    {
        return SystemLanguage {
            language,
            requested_locale: Some(tag.clone()),
            source: SystemLanguageSource::PreferredUiLanguages,
        };
    }
    // SAFETY: no parameters or output buffers; returns the user's UI LANGID.
    let ui_language = unsafe { GetUserDefaultUILanguage() };
    // SAFETY: locale_name supplies a fixed, valid UTF-16 output buffer.
    let ui_tag =
        locale_name(|buffer| unsafe { LCIDToLocaleName(u32::from(ui_language), Some(buffer), 0) });
    if let Some(tag) = ui_tag.as_deref()
        && let Some(language) = Language::from_locale(tag)
    {
        return SystemLanguage {
            language,
            requested_locale: Some(tag.into()),
            source: SystemLanguageSource::UserUiLanguage,
        };
    }
    // SAFETY: valid bounded output buffer; read-only Windows locale query.
    let region_tag = locale_name(|buffer| unsafe { GetUserDefaultLocaleName(buffer) });
    if let Some(tag) = region_tag.as_deref()
        && let Some(language) = Language::from_locale(tag)
    {
        return SystemLanguage {
            language,
            requested_locale: Some(tag.into()),
            source: SystemLanguageSource::UserLocale,
        };
    }
    SystemLanguage {
        language: Language::EnUs,
        requested_locale: first.or(ui_tag).or(region_tag),
        source: SystemLanguageSource::EnglishFallback,
    }
}

static COMPONENTS: Once = Once::new();
/// Register framework overrides once from the GUI binary after Kit init.
/// Pass `gpui_kit::component::_rust_i18n_extend`. Keeping this callback at the
/// binary boundary prevents GPUI's manifest and resources reaching the MSI helper.
pub fn register_components(register: impl FnOnce(&'static dyn rust_i18n::Backend, &'static str)) {
    COMPONENTS.call_once(|| register(crate::_rust_i18n_backend(), "gpui_component"));
}
/// Apply on the UI thread; the caller must notify/relabel its existing views.
/// Returns the resolved language. Persistence belongs to the settings writer.
pub fn set_language(preference: Language) -> Language {
    let language = preference.resolve();
    // GPUI components use this same rust-i18n global without a GUI dependency here.
    rust_i18n::set_locale(language.tag());
    language
}
pub fn current_language() -> Language {
    Language::from_locale(&rust_i18n::locale()).unwrap_or(Language::EnUs)
}
fn lookup(language: Language, key: &str) -> Option<Cow<'static, str>> {
    let language = language.resolve();
    let backend = crate::_rust_i18n_backend();
    backend
        .translate(language.tag(), key)
        .or_else(|| backend.translate(Language::EnUs.tag(), key))
}
pub fn text(key: &str) -> Cow<'static, str> {
    text_in(current_language(), key)
}
/// Explicit-locale lookup does not change global language; safe for workers/tests.
pub fn text_in(language: Language, key: &str) -> Cow<'static, str> {
    lookup(language, key).unwrap_or_else(|| Cow::Owned(key.to_owned()))
}
fn source_keys() -> &'static HashMap<String, String> {
    static KEYS: OnceLock<HashMap<String, String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/source-aliases.data"))
            .expect("compiled app-owned source key map")
    })
}
pub fn source_key(source: &str) -> Option<&'static str> {
    source_keys().get(source).map(String::as_str)
}
/// Only use for a known app-owned literal, never a process/device/path/error value.
/// Dynamic templates must use explicit named format_text arguments.
pub fn text_source(source: &str) -> Cow<'static, str> {
    source_key(source)
        .map(text)
        .unwrap_or_else(|| Cow::Owned(source.to_owned()))
}
pub fn text_source_in(language: Language, source: &str) -> Cow<'static, str> {
    source_key(source)
        .map(|key| text_in(language, key))
        .unwrap_or_else(|| Cow::Owned(source.to_owned()))
}

/// Source templates are only a migration bridge for app-owned compile-time literals.
/// Missing catalog entries retain their original template for development; call
/// validate_used_sources before shipping, so this never masquerades as complete UI.
pub fn format_source(source: &'static str, arguments: &[(&str, &str)]) -> Result<String> {
    format_source_in(current_language(), source, arguments)
}
pub fn format_source_in(
    language: Language,
    source: &'static str,
    arguments: &[(&str, &str)],
) -> Result<String> {
    if let Some(key) = source_key(source) {
        format_text_in(language, key, arguments)
    } else {
        Ok(source.to_owned())
    }
}
pub fn validate_used_sources(sources: &[&str]) -> Result<()> {
    let backend = crate::_rust_i18n_backend();
    for source in sources {
        let key = source_key(source).ok_or_else(|| {
            anyhow::anyhow!("App-owned literal missing from translation catalog: {source}")
        })?;
        let english = backend
            .translate(Language::EnUs.tag(), key)
            .ok_or_else(|| anyhow::anyhow!("English translation missing: {key}"))?;
        let expected = placeholders(&english)?;
        for language in Language::SUPPORTED {
            let value = backend
                .translate(language.tag(), key)
                .ok_or_else(|| anyhow::anyhow!("{} translation missing: {key}", language.tag()))?;
            ensure!(
                !value.trim().is_empty() && placeholders(&value)? == expected,
                "Invalid {} translation: {key}",
                language.tag()
            );
        }
    }
    Ok(())
}
fn placeholders(template: &str) -> Result<BTreeSet<&str>> {
    let mut names = BTreeSet::new();
    let mut tail = template;
    while let Some(start) = tail.find("%{") {
        tail = &tail[start + 2..];
        let end = tail
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("Unclosed translation placeholder"))?;
        let name = &tail[..end];
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "Invalid named translation placeholder: {name}"
        );
        names.insert(name);
        tail = &tail[end + 1..];
    }
    Ok(names)
}
pub fn format_text(key: &str, arguments: &[(&str, &str)]) -> Result<String> {
    format_text_in(current_language(), key, arguments)
}
/// Strict named substitution. Values are inserted once and never translated,
/// scanned as new placeholders or interpreted as format/control code.
pub fn format_text_in(language: Language, key: &str, arguments: &[(&str, &str)]) -> Result<String> {
    let template =
        lookup(language, key).ok_or_else(|| anyhow::anyhow!("Unknown translation key: {key}"))?;
    let expected = placeholders(&template)?;
    let mut supplied = HashMap::with_capacity(arguments.len());
    for &(name, value) in arguments {
        ensure!(
            supplied.insert(name, value).is_none(),
            "Duplicate translation argument: {name}"
        );
    }
    ensure!(
        expected.len() == supplied.len() && expected.iter().all(|name| supplied.contains_key(name)),
        "Translation arguments differ from placeholders for {key}"
    );
    let mut output = String::with_capacity(template.len() + 32);
    let mut tail = template.as_ref();
    while let Some(start) = tail.find("%{") {
        output.push_str(&tail[..start]);
        tail = &tail[start + 2..];
        let Some(end) = tail.find('}') else {
            bail!("Unclosed translation placeholder");
        };
        output.push_str(supplied[&tail[..end]]);
        tail = &tail[end + 1..];
    }
    output.push_str(tail);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{
        Language, SystemLanguageSource, current_language, detect_system_language, format_text_in,
        parse_language_list, placeholders, register_components, set_language, source_key, text_in,
        text_source_in,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeSet;

    #[test]
    fn settings_unknown_language_never_resets_other_fields() {
        #[derive(Deserialize, Serialize)]
        struct Settings {
            #[serde(default)]
            language: Language,
            interval: u64,
            dark: bool,
        }
        for field in [
            "",
            ",\"language\":\"future-ZZ\"",
            ",\"language\":null",
            ",\"language\":123",
            ",\"language\":true",
            ",\"language\":{}",
        ] {
            let settings: Settings =
                serde_json::from_str(&format!("{{\"interval\":2000,\"dark\":false{field}}}"))
                    .unwrap();
            assert_eq!(settings.language, Language::System);
            assert_eq!(settings.interval, 2000);
            assert!(!settings.dark);
        }
        for language in Language::ALL {
            let json = serde_json::to_string(&language).unwrap();
            assert_eq!(serde_json::from_str::<Language>(&json).unwrap(), language);
            assert_eq!(json, format!("\"{}\"", language.tag()));
        }
    }
    #[test]
    fn supported_locale_families_and_system_preferences_are_deterministic() {
        for (tag, expected) in [
            ("EN_gb", Language::EnUs),
            ("zh-Hant-TW", Language::ZhCn),
            ("pt-PT", Language::PtBr),
            ("ja", Language::JaJp),
            ("ko-KR", Language::KoKr),
            ("de-DE", Language::DeDe),
            ("fr-CA", Language::FrFr),
            ("es-MX", Language::EsEs),
        ] {
            assert_eq!(Language::from_locale(tag), Some(expected));
        }
        for tag in ["system", "ru-RU", "", "garbage"] {
            assert_eq!(Language::from_preference(tag), Language::System);
        }
        assert_eq!(Language::SUPPORTED.len(), 8);
        let detected = detect_system_language();
        assert!(Language::SUPPORTED.contains(&detected.language));
        if detected.source != SystemLanguageSource::EnglishFallback {
            assert!(detected.requested_locale.is_some());
        }
    }
    #[test]
    fn native_multisz_parser_rejects_truncation_bad_utf16_and_counts() {
        let valid: Vec<_> = "ja-JP\0en-US\0\0".encode_utf16().collect();
        assert_eq!(parse_language_list(&valid, 2).unwrap(), ["ja-JP", "en-US"]);
        assert!(parse_language_list(&valid[..valid.len() - 1], 2).is_none());
        assert!(parse_language_list(&valid, 3).is_none());
        assert!(parse_language_list(&[0xd800, 0, 0], 1).is_none());
        assert!(parse_language_list(&valid, 0).is_none());
        assert!(parse_language_list(&valid, u32::MAX).is_none());
    }
    #[test]
    fn every_compiled_key_has_all_eight_locales_and_matching_parameters() {
        let backend = crate::_rust_i18n_backend();
        let english = backend.messages_for_locale("en-US").unwrap();
        assert!(!english.is_empty());
        let expected: BTreeSet<_> = english.iter().map(|(key, _)| key.as_ref()).collect();
        for language in Language::SUPPORTED {
            let messages = backend.messages_for_locale(language.tag()).unwrap();
            let actual: BTreeSet<_> = messages.iter().map(|(key, _)| key.as_ref()).collect();
            assert_eq!(actual, expected, "{} key coverage", language.tag());
            for (key, english_text) in &english {
                let translated = backend.translate(language.tag(), key).unwrap();
                assert!(
                    !translated.trim().is_empty(),
                    "{key} {} is empty",
                    language.tag()
                );
                assert_eq!(
                    placeholders(&translated).unwrap(),
                    placeholders(english_text).unwrap(),
                    "{key} {} parameters",
                    language.tag()
                );
            }
        }
    }
    #[test]
    fn named_arguments_are_strict_and_real_values_are_not_reinterpreted() {
        let literal = "C:\\用户\\é 日本語\\%{count}\\app.exe";
        let formatted = format_text_in(
            Language::JaJp,
            "i18n.system_resolved",
            &[("language", literal)],
        )
        .unwrap();
        assert!(formatted.contains(literal));
        assert!(format_text_in(Language::EnUs, "i18n.system_resolved", &[]).is_err());
        assert!(
            format_text_in(
                Language::EnUs,
                "i18n.system_resolved",
                &[("language", "a"), ("language", "b")]
            )
            .is_err()
        );
        assert!(format_text_in(Language::EnUs, "i18n.system_resolved", &[("wrong", "a")]).is_err());
        assert!(format_text_in(Language::EnUs, "missing.key", &[]).is_err());
        assert!(placeholders("%{missing").is_err());
    }
    #[test]
    fn source_literal_map_only_translates_known_app_literals() {
        assert_eq!(source_key("进程"), Some("nav.processes"));
        assert_eq!(text_source_in(Language::EnUs, "进程"), "Processes");
        assert_eq!(
            text_source_in(Language::JaJp, "C:\\用户\\device.exe"),
            "C:\\用户\\device.exe"
        );
        assert_eq!(text_in(Language::EnUs, "missing.key"), "missing.key");
    }
    #[test]
    fn full_source_catalog_validates_and_formats_mapped_rust_templates() {
        let aliases = super::source_keys();
        assert!(aliases.len() >= 456);
        let sources: Vec<_> = aliases.keys().map(String::as_str).collect();
        super::validate_used_sources(&sources).unwrap();
        assert!(
            super::validate_used_sources(&["A deliberately missing app-owned literal"]).is_err()
        );
        let (source, key) = aliases
            .iter()
            .find(|(_, key)| {
                !placeholders(&text_in(Language::EnUs, key))
                    .unwrap()
                    .is_empty()
            })
            .expect("at least one reviewed dynamic source template");
        let template = text_in(Language::EnUs, key);
        let names = placeholders(&template).unwrap();
        let literal = "C:\\用户\\日本語\\%{arg0}\\app.exe";
        let arguments: Vec<_> = names.iter().map(|name| (*name, literal)).collect();
        for language in Language::SUPPORTED {
            let formatted = super::format_source_in(language, source, &arguments).unwrap();
            assert!(formatted.contains(literal));
        }
        assert!(super::format_source_in(Language::EnUs, source, &[]).is_err());
        assert_eq!(
            super::format_source_in(Language::EnUs, "Unmapped development template", &[]).unwrap(),
            "Unmapped development template"
        );
    }

    #[test]
    fn sequential_language_switches_share_framework_defaults_and_register_once() {
        let original = current_language();
        register_components(gpui_kit::component::_rust_i18n_extend);
        register_components(gpui_kit::component::_rust_i18n_extend);
        for language in Language::SUPPORTED {
            assert_eq!(set_language(language), language);
            assert_eq!(current_language(), language);
            assert_eq!(&*gpui_kit::component::locale(), language.tag());
            for key in [
                "Dialog.ok",
                "Dialog.cancel",
                "Input.Copy",
                "Input.Select All",
                "Select.placeholder",
                "ComboBox.empty",
            ] {
                let translated =
                    gpui_kit::component::_rust_i18n_try_translate(language.tag(), key).unwrap();
                assert_eq!(
                    translated,
                    text_in(language, &format!("gpui_component.{key}"))
                );
            }
        }
        set_language(original);
    }
}

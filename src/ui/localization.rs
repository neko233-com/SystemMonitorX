use gpui_kit::SharedString;
use system_monitor_x::i18n;

/// Only application-owned source literals and labels may pass this boundary.
/// Process/device names, paths and raw external diagnostics stay unchanged.
pub(super) fn tr(source: &str) -> SharedString {
    i18n::text_source(source).as_ref().into()
}
pub(super) fn trs(source: &str) -> String {
    i18n::text_source(source).into_owned()
}
pub(super) fn tf(source: &'static str, args: &[(&str, String)]) -> String {
    let values: Vec<_> = args
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    if i18n::source_key(source).is_none() {
        return format_fallback(source, args);
    }
    i18n::format_source(source, &values).unwrap_or_else(|_| format_fallback(source, args))
}

pub(super) fn trs_in(language: i18n::Language, source: &str) -> String {
    i18n::text_source_in(language, source).into_owned()
}
pub(super) fn tf_in(
    language: i18n::Language,
    source: &'static str,
    args: &[(&str, String)],
) -> String {
    let values: Vec<_> = args
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    if i18n::source_key(source).is_none() {
        return format_fallback(source, args);
    }
    i18n::format_source_in(language, source, &values)
        .unwrap_or_else(|_| format_fallback(source, args))
}

/// Cached feedback retains source keys and raw values so locale changes do not
/// discard errors, reread hardware or translate external process data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UiMessage {
    Source(String),
    Template {
        source: &'static str,
        args: Vec<(String, UiMessage)>,
    },
    Progress(String),
    Raw(String),
}
impl UiMessage {
    pub(super) fn render(&self) -> String {
        self.render_in(i18n::current_language())
    }
    pub(super) fn render_in(&self, language: i18n::Language) -> String {
        match self {
            Self::Source(source) => trs_in(language, source),
            Self::Raw(value) => value.clone(),
            Self::Progress(source) => format!("{}…", trs_in(language, source)),
            Self::Template { source, args } => {
                let values: Vec<_> = args
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.render_in(language)))
                    .collect();
                tf_in(language, source, &values)
            }
        }
    }
}
impl From<String> for UiMessage {
    fn from(value: String) -> Self {
        Self::Raw(value)
    }
}
pub(super) fn msg(source: &str) -> UiMessage {
    UiMessage::Source(source.to_owned())
}
pub(super) fn mf(source: &'static str, args: &[(&str, String)]) -> UiMessage {
    mlf(
        source,
        &args
            .iter()
            .map(|(name, value)| (*name, value.clone().into()))
            .collect::<Vec<_>>(),
    )
}
pub(super) fn mlf(source: &'static str, args: &[(&str, UiMessage)]) -> UiMessage {
    UiMessage::Template {
        source,
        args: args
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect(),
    }
}

/// Known application notes translate; external diagnostics remain verbatim
/// behind a translated label. Never use this for names or ordinary values.
pub(super) fn diagnostic(source: &str) -> String {
    if i18n::source_key(source).is_some() {
        trs(source)
    } else {
        tf("原始诊断：{issue}", &[("issue", source.to_owned())])
    }
}

/// Only the application's process-network status may enter this parser.
/// Four ETW operation labels and canonical u32 codes are recognized; arbitrary
/// OS diagnostics, names and malformed text fall back unchanged to diagnostic.
pub(super) fn network_diagnostic(source: &str) -> String {
    network_trace_message(source)
        .map(|message| message.render())
        .unwrap_or_else(|| diagnostic(source))
}

fn network_trace_message(source: &str) -> Option<UiMessage> {
    let body = source.strip_prefix("逐进程网络不可用：")?;
    let (action, error, denied) = if let Some((action, rest)) = body.split_once('被') {
        let rest = rest.strip_prefix(" Windows 拒绝（")?;
        let (error, tail) = rest.split_once('）')?;
        if !tail
            .strip_prefix("；需要管理员或已授予的 ETW 性能追踪权限")?
            .is_empty()
        {
            return None;
        }
        (action, error, true)
    } else {
        let (action, rest) = body.split_once('，')?;
        (action, rest.strip_prefix("Windows 错误 ")?, false)
    };
    if !matches!(
        action,
        "启动网络追踪" | "启用网络提供者" | "打开网络事件流" | "网络事件流已停止"
    ) || error.is_empty()
        || !error.bytes().all(|value| value.is_ascii_digit())
    {
        return None;
    }
    let code = error.parse::<u32>().ok()?;
    // trace_error formats canonical u32 decimal and selects the permission
    // sentence only for ERROR_ACCESS_DENIED / ERROR_PRIVILEGE_NOT_HELD.
    if code.to_string() != error || denied != matches!(code, 5 | 1314) {
        return None;
    }
    let args = [("action", msg(action)), ("error", error.to_owned().into())];
    Some(if denied {
        mlf(
            "逐进程网络不可用：{action}被 Windows 拒绝（{error}）；需要管理员或已授予的 ETW 性能追踪权限",
            &args,
        )
    } else {
        mlf("逐进程网络不可用：{action}，Windows 错误 {error}", &args)
    })
}

fn format_fallback(source: &str, args: &[(&str, String)]) -> String {
    // Keep raw diagnostics even if a developer introduces a broken template.
    format!(
        "{}\n{}",
        source,
        args.iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_feedback_changes_language_without_changing_raw_diagnostics() {
        let raw = r"C:\国际-é-日本\app.exe: OS error 5; %{pid}".to_owned();
        let message = mf("操作失败：{error:#}", &[("error", raw.clone())]);
        let zh = message.render_in(i18n::Language::ZhCn);
        let en = message.render_in(i18n::Language::EnUs);
        assert_ne!(zh, en);
        assert!(en.contains(&raw));
        assert!(zh.contains(&raw));
        assert_eq!(message.render_in(i18n::Language::EnUs), en);
    }
    #[test]
    fn malformed_template_keeps_external_diagnostic_in_fallback() {
        let raw = "OS error 5, real %{error} path".to_owned();
        let result = tf_in(
            i18n::Language::EnUs,
            "操作失败：{error:#}",
            &[("incorrect", raw.clone())],
        );
        assert!(result.contains(&raw));
    }
    #[test]
    fn nested_owned_labels_are_translated_and_raw_names_are_preserved() {
        let message = mlf("优先级已设为{name}", &[("name", msg("高"))]);
        for language in i18n::Language::SUPPORTED {
            let result = message.render_in(language);
            assert!(!result.contains("%{"));
            assert!(result.contains(&trs_in(language, "高")));
        }
        let real_name: UiMessage = "运行中.exe".to_owned().into();
        assert_eq!(real_name.render_in(i18n::Language::EnUs), "运行中.exe");
    }
    #[test]
    fn network_permission_diagnostics_translate_eight_languages_and_keep_unknown_raw_text() {
        let actions = [
            "启动网络追踪",
            "启用网络提供者",
            "打开网络事件流",
            "网络事件流已停止",
        ];
        for language in i18n::Language::SUPPORTED {
            for action in actions {
                for error in [5_u32, 1314] {
                    let source = format!(
                        "逐进程网络不可用：{action}被 Windows 拒绝（{error}）；需要管理员或已授予的 ETW 性能追踪权限"
                    );
                    let translated = network_trace_message(&source)
                        .expect("known ETW permission status")
                        .render_in(language);
                    assert!(translated.contains(&trs_in(language, action)));
                    assert!(translated.contains(&error.to_string()));
                    assert!(!translated.contains("%{"));
                    if language != i18n::Language::ZhCn {
                        assert_ne!(translated, source);
                    }
                }
                let source = format!("逐进程网络不可用：{action}，Windows 错误 4294967295");
                let translated = network_trace_message(&source)
                    .expect("canonical maximum u32 error")
                    .render_in(language);
                assert!(translated.contains("4294967295"));
                assert!(translated.contains(&trs_in(language, action)));
            }
        }
        let unknown = r"C:\国际-é-日本\app.exe: OS error 5; %{action}";
        assert!(network_trace_message(unknown).is_none());
        assert!(network_diagnostic(unknown).ends_with(unknown));
        for malformed in [
            "逐进程网络不可用：未知操作，Windows 错误 87",
            "逐进程网络不可用：启动网络追踪，Windows 错误 4294967296",
            "逐进程网络不可用：启动网络追踪，Windows 错误 +87",
            "逐进程网络不可用：启动网络追踪，Windows 错误 087",
            "逐进程网络不可用：启动网络追踪，Windows 错误 87 extra",
            "逐进程网络不可用：启动网络追踪，Windows 错误 5",
            "逐进程网络不可用：启动网络追踪被 Windows 拒绝（5）；需要管理员或已授予的 ETW 性能追踪权限 extra",
            "逐进程网络不可用：启动网络追踪被 Windows 拒绝（87）；需要管理员或已授予的 ETW 性能追踪权限",
        ] {
            assert!(network_trace_message(malformed).is_none());
            assert!(network_diagnostic(malformed).ends_with(malformed));
        }
    }
}

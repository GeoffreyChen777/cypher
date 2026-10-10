//! The page's pure model: the custom gateway kinds, form validation and the
//! status and title labels a provider row shows.

use cypher_engine::pi::providers::PiProviderInfo;

use crate::kit::icons;

/// Kinds the Add-provider dropdown can create. Claude Code and ChatGPT are
/// listed separately; this catalog is only custom gateways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CustomProviderKind {
    NewApi,
}

pub(super) struct CustomProviderKindSpec {
    pub(super) title: &'static str,
    pub(super) caption: &'static str,
    pub(super) icon: &'static str,
}

impl CustomProviderKind {
    /// Add new custom types here; the dropdown renders this list in order.
    pub(super) const ALL: &[Self] = &[Self::NewApi];

    pub(super) fn spec(self) -> CustomProviderKindSpec {
        match self {
            Self::NewApi => CustomProviderKindSpec {
                title: "OpenAI-compatible",
                caption: "NewAPI and compatible gateways",
                icon: icons::GLOBAL,
            },
        }
    }

    pub(super) fn from_provider(provider: Option<&PiProviderInfo>) -> Self {
        // Only one provider type exists today, and an unknown one still gets
        // the NewApi form rather than a blank panel.
        let _ = provider;
        Self::NewApi
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Name,
    Url,
    Key,
}

#[derive(Default)]
pub(super) struct FieldErrors {
    pub(super) name: Option<&'static str>,
    pub(super) url: Option<&'static str>,
    pub(super) key: Option<&'static str>,
}

impl FieldErrors {
    pub(super) fn first(&self) -> Option<Field> {
        if self.name.is_some() {
            Some(Field::Name)
        } else if self.url.is_some() {
            Some(Field::Url)
        } else if self.key.is_some() {
            Some(Field::Key)
        } else {
            None
        }
    }
}

/// Match the service's URL policy so avoidable mistakes stay next to the field.
/// The engine remains authoritative; client validation is not a security gate.
pub(super) fn normalized_url(value: &str) -> Option<String> {
    if value.contains(['\r', '\n']) {
        return None;
    }
    let mut url = url::Url::parse(value.trim()).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return None;
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(path.strip_suffix("/v1").unwrap_or(&path));
    Some(url.to_string().trim_end_matches('/').into())
}

pub(super) fn validate_form(
    name: &str,
    url: &str,
    key: &str,
    original: Option<&PiProviderInfo>,
) -> FieldErrors {
    let mut errors = FieldErrors::default();
    let name_ok = !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && !matches!(name, "constructor" | "prototype" | "__proto__");
    if !name_ok {
        errors.name = Some("Use 1–64 letters, numbers, dots, hyphens or underscores.");
    }
    let normalized = normalized_url(url);
    if normalized.is_none() {
        errors.url = Some(
            "Enter an HTTPS service URL without credentials or query parameters. Localhost may use HTTP.",
        );
    }
    let can_keep_key = original.is_some_and(|p| {
        p.credential_saved && normalized.is_some() && normalized_url(&p.base_url) == normalized
    });
    if key.contains(['\r', '\n']) {
        errors.key = Some("Paste a single API key, without line breaks.");
    } else if key.is_empty() && !can_keep_key {
        errors.key = Some(if original.is_some_and(|p| p.credential_saved) {
            "Enter the API key again when changing the service URL."
        } else {
            "Enter an API key to connect this provider."
        });
    }
    errors
}

pub(super) fn status_label(provider: &PiProviderInfo) -> &'static str {
    match provider.state.as_str() {
        "connected" if is_claude_cli(provider) => "Installed",
        "connected" => "Verified",
        "error" => "Connection failed",
        "signed_out" if is_claude_cli(provider) => "Needs Claude Code",
        "signed_out" if provider.provider_type == "oauth" => "Needs sign-in",
        "signed_out" => "Needs API key",
        _ => "Not verified",
    }
}

pub(super) fn is_oauth(provider: &PiProviderInfo) -> bool {
    provider.provider_type == "oauth"
}

pub(super) fn is_claude_cli(provider: &PiProviderInfo) -> bool {
    provider.provider_type == "claude-cli"
}

pub(super) fn is_subscription(provider: &PiProviderInfo) -> bool {
    is_oauth(provider) || is_claude_cli(provider)
}

pub(super) fn provider_title(provider: &PiProviderInfo) -> String {
    provider
        .title
        .clone()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| provider.id.clone())
}

pub(super) fn checked_label(at: Option<i64>, now: i64) -> String {
    let Some(at) = at else {
        return "Not checked yet".into();
    };
    let minutes = now.saturating_sub(at).max(0) / 60_000;
    match minutes {
        0 => "Checked just now".into(),
        1..=59 => format!("Checked {minutes}m ago"),
        60..=1439 => format!("Checked {}h ago", minutes / 60),
        _ => chrono::DateTime::from_timestamp_millis(at)
            .map(|d| {
                format!(
                    "Checked {}",
                    d.with_timezone(&chrono::Local).format("%b %-d")
                )
            })
            .unwrap_or_else(|| "Not checked yet".into()),
    }
}

//! Operator-declared provider connection profiles.
//!
//! A profile records only a name, a provider kind, and an authentication mode.
//! It stores no endpoint, account, token, password, or other credential, and
//! no adapter uses it yet. It is configuration only. See
//! `docs/provider-authentication.md` and ADR-0038.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_profile_name, valid_uuid};

/// Most recorded provider profiles.
pub const MAX_PROVIDER_PROFILES: usize = 32;

/// A provider that a profile may name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderKind {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "ollama_local")]
    OllamaLocal,
    #[serde(rename = "ollama_cloud")]
    OllamaCloud,
    #[serde(rename = "open_router")]
    OpenRouter,
    #[serde(rename = "lm_studio")]
    LmStudio,
}

impl ProviderKind {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::OllamaLocal => "ollama_local",
            Self::OllamaCloud => "ollama_cloud",
            Self::OpenRouter => "open_router",
            Self::LmStudio => "lm_studio",
        }
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            "ollama_local" => Some(Self::OllamaLocal),
            "ollama_cloud" => Some(Self::OllamaCloud),
            "open_router" => Some(Self::OpenRouter),
            "lm_studio" => Some(Self::LmStudio),
            _ => None,
        }
    }
}

/// How a provider connection would authenticate, when implemented.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuth {
    /// A provider-supported subscription sign-in.
    Subscription,
    /// A separately billed API key.
    ApiKey,
    /// No provider credential; a local or unauthenticated endpoint.
    None,
}

impl ProviderAuth {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::ApiKey => "api_key",
            Self::None => "none",
        }
    }

    /// Parse a stored or command-line spelling, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "subscription" => Some(Self::Subscription),
            "api_key" | "api-key" => Some(Self::ApiKey),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// One recorded provider connection shape. Credentials are never stored here.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    id: Uuid,
    name: String,
    kind: ProviderKind,
    auth: ProviderAuth,
}

impl ProviderProfile {
    /// Stable profile identifier.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The operator's name for the profile.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provider kind.
    pub const fn kind(&self) -> ProviderKind {
        self.kind
    }

    /// The authentication mode.
    pub const fn auth(&self) -> ProviderAuth {
        self.auth
    }
}

/// A profile and the settings revision that recorded it.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderProfileStatus {
    #[serde(flatten)]
    profile: ProviderProfile,
    revision: u64,
}

impl ProviderProfileStatus {
    /// The recorded profile.
    pub const fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    /// The settings revision of the event that added it.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// An operator's provider profile declaration, before parsing.
#[derive(Clone, Debug, Default)]
pub struct ProviderProfileDeclaration {
    /// The profile name, with the same rules as other profile names.
    pub name: String,
    /// A provider spelling such as `openai` or `ollama_local`.
    pub kind: String,
    /// `subscription`, `api_key`, or `none`.
    pub auth: String,
}

impl ControlStore {
    /// Every recorded provider profile, in the order they were added.
    pub fn provider_profiles(&self) -> &[ProviderProfileStatus] {
        &self.snapshot.provider_profiles
    }

    /// The profile with this name, compared case-insensitively.
    pub fn provider_profile(&self, name: &str) -> Option<&ProviderProfileStatus> {
        self.snapshot
            .provider_profiles
            .iter()
            .find(|status| status.profile.name.eq_ignore_ascii_case(name))
    }

    /// Record a provider connection shape. This advances the settings revision
    /// and records no credential or runtime authority.
    pub fn add_provider_profile(
        &mut self,
        declaration: &ProviderProfileDeclaration,
        expected_revision: u64,
    ) -> Result<ProviderProfile, ControlError> {
        self.check_revision(expected_revision)?;
        if self.provider_profiles().len() >= MAX_PROVIDER_PROFILES {
            return Err(ControlError::ProviderRefused(
                "at most 32 provider profiles may be recorded",
            ));
        }
        if self.provider_profile(&declaration.name).is_some() {
            return Err(ControlError::ProviderRefused(
                "a provider profile with this name already exists; remove it first",
            ));
        }
        let kind = ProviderKind::parse(&declaration.kind)
            .ok_or(ControlError::ProviderRefused("unknown provider kind"))?;
        let auth = ProviderAuth::parse(&declaration.auth).ok_or(ControlError::ProviderRefused(
            "authentication must be subscription, api_key, or none",
        ))?;
        let profile = ProviderProfile {
            id: Uuid::new_v4(),
            name: declaration.name.clone(),
            kind,
            auth,
        };
        validate_profile(&profile)?;
        self.append(
            Action::ProviderProfileAdded {
                profile: profile.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(profile)
    }

    /// Remove a provider profile by name. This advances the settings revision.
    pub fn remove_provider_profile(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Uuid, ControlError> {
        self.check_revision(expected_revision)?;
        let profile_id = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?
            .profile
            .id;
        self.append(
            Action::ProviderProfileRemoved { profile_id },
            self.next_revision()?,
        )?;
        Ok(profile_id)
    }
}

/// Validate and apply one provider action during replay or append preflight.
pub(super) fn apply(
    state: &mut ControlSnapshot,
    action: &Action,
    revision: u64,
) -> Result<(), ControlError> {
    match action {
        Action::ProviderProfileAdded { profile } => {
            validate_profile(profile)
                .map_err(|_| ControlError::Invalid("invalid provider profile"))?;
            if state.provider_profiles.len() >= MAX_PROVIDER_PROFILES
                || state.provider_profiles.iter().any(|status| {
                    status.profile.id == profile.id
                        || status.profile.name.eq_ignore_ascii_case(&profile.name)
                })
            {
                return Err(ControlError::Invalid(
                    "duplicate provider profile or too many provider profiles",
                ));
            }
            state.provider_profiles.push(ProviderProfileStatus {
                profile: profile.clone(),
                revision,
            });
        }
        Action::ProviderProfileRemoved { profile_id } => {
            let index = state
                .provider_profiles
                .iter()
                .position(|status| status.profile.id == *profile_id)
                .ok_or(ControlError::Invalid(
                    "removal of an unknown provider profile",
                ))?;
            state.provider_profiles.remove(index);
        }
        _ => {}
    }
    Ok(())
}

/// Check a profile name and the provider/authentication pairing.
fn validate_profile(profile: &ProviderProfile) -> Result<(), ControlError> {
    if !valid_uuid(profile.id) || !valid_profile_name(&profile.name) {
        return Err(ControlError::ProviderRefused(
            "provider profile names are 1 to 64 ASCII letters, digits, '.', '-', or '_', starting with a letter or digit",
        ));
    }
    if !auth_allowed(profile.kind, profile.auth) {
        return Err(ControlError::ProviderRefused(
            "this provider does not support the selected authentication mode",
        ));
    }
    Ok(())
}

fn auth_allowed(kind: ProviderKind, auth: ProviderAuth) -> bool {
    match kind {
        ProviderKind::OpenAi | ProviderKind::Anthropic => {
            matches!(auth, ProviderAuth::Subscription | ProviderAuth::ApiKey)
        }
        ProviderKind::OllamaCloud | ProviderKind::OpenRouter => auth == ProviderAuth::ApiKey,
        ProviderKind::OllamaLocal => auth == ProviderAuth::None,
        ProviderKind::LmStudio => matches!(auth, ProviderAuth::None | ProviderAuth::ApiKey),
    }
}

#[cfg(test)]
mod tests;

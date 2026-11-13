//! Operator-declared package-network profiles.
//!
//! A package-network profile names registry origins a future package-fetch
//! mechanism may contact. It is configuration only: no run accepts a profile,
//! and recording one never authorizes network access. The runtime has no
//! destination filter. See `docs/execution.md` and ADR-0034.
//!
//! Origins are stored as parsed fields rather than a free-form URL so replay
//! can check every value without network or DNS. `https` is required except
//! for a loopback `http` test fixture.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_profile_name, valid_uuid};

/// Most recorded package-network profiles.
pub const MAX_PACKAGE_PROFILES: usize = 32;
/// Most registry origins in one profile.
pub const MAX_PACKAGE_ORIGINS: usize = 8;
/// Longest registry origin text, in bytes.
pub const MAX_PACKAGE_ORIGIN_BYTES: usize = 2048;
/// Longest registry host, in bytes.
pub const MAX_PACKAGE_HOST_BYTES: usize = 253;
/// Longest registry path prefix, in bytes.
pub const MAX_PACKAGE_PATH_BYTES: usize = 1024;

/// The scheme a registry origin uses.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageScheme {
    /// TLS is required.
    Https,
    /// Plain HTTP, allowed only for a loopback test fixture.
    Http,
}

impl PackageScheme {
    /// The scheme as it appears in a URL.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
        }
    }
}

/// One registry origin. No credentials, query, fragment, or wildcard is kept.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageOrigin {
    scheme: PackageScheme,
    host: String,
    port: u16,
    /// A path prefix that starts and ends with `/`.
    path_prefix: String,
}

impl PackageOrigin {
    /// The origin's scheme.
    pub const fn scheme(&self) -> PackageScheme {
        self.scheme
    }

    /// The origin's host, as the URL parser reports it. An IPv6 literal keeps
    /// its brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The origin's explicit port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The origin's normalized path prefix.
    pub fn path_prefix(&self) -> &str {
        &self.path_prefix
    }
}

/// A recorded package-network profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageProfile {
    id: Uuid,
    name: String,
    origins: Vec<PackageOrigin>,
}

impl PackageProfile {
    /// Stable profile identifier.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The operator's name for the profile.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Registry origins, in declaration order.
    pub fn origins(&self) -> &[PackageOrigin] {
        &self.origins
    }
}

/// A profile and the settings revision that recorded it.
#[derive(Clone, Debug, Serialize)]
pub struct PackageProfileStatus {
    #[serde(flatten)]
    profile: PackageProfile,
    revision: u64,
}

impl PackageProfileStatus {
    /// The recorded profile.
    pub const fn profile(&self) -> &PackageProfile {
        &self.profile
    }

    /// The settings revision of the event that added it.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// An operator's profile declaration, before any origin is parsed.
#[derive(Clone, Debug, Default)]
pub struct PackageProfileDeclaration {
    /// The profile name, with the same rules as a toolchain profile name.
    pub name: String,
    /// Registry origins. At least one.
    pub origins: Vec<String>,
}

/// Parse one registry origin.
///
/// The origin must be an absolute `https` URL. `http` is accepted only for
/// `localhost`, `127.0.0.1`, or `::1`. User information, a query, and a
/// fragment are refused. The path is normalized to start and end with `/`.
///
/// # Errors
///
/// Returns [`ControlError::PackageProfileRefused`] for a rejected origin.
pub fn parse_origin(text: &str) -> Result<PackageOrigin, ControlError> {
    if text.len() > MAX_PACKAGE_ORIGIN_BYTES {
        return Err(refused("a registry origin is too long"));
    }
    let url = reqwest::Url::parse(text)
        .map_err(|_| refused("a registry origin must be an absolute URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused("a registry origin must not contain credentials"));
    }
    if url.query().is_some() {
        return Err(refused("a registry origin must not contain a query"));
    }
    if url.fragment().is_some() {
        return Err(refused("a registry origin must not contain a fragment"));
    }
    let scheme = match url.scheme() {
        "https" => PackageScheme::Https,
        "http" => PackageScheme::Http,
        _ => return Err(refused("a registry origin must use https")),
    };
    let host = url
        .host_str()
        .ok_or_else(|| refused("a registry origin needs a host"))?;
    if host.is_empty() || host.len() > MAX_PACKAGE_HOST_BYTES || !host.is_ascii() {
        return Err(refused("a registry host is invalid or too long"));
    }
    if host.ends_with('.') {
        return Err(refused("a registry host must not end with a dot"));
    }
    if scheme == PackageScheme::Http && !is_loopback_host(host) {
        return Err(refused("http is allowed only for a loopback test fixture"));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| refused("a registry origin needs a port"))?;
    if port == 0 {
        return Err(refused("a registry origin needs a nonzero port"));
    }
    let mut path_prefix = url.path().to_owned();
    if path_prefix.is_empty() {
        path_prefix.push('/');
    }
    if !path_prefix.starts_with('/') {
        return Err(refused("a registry path must start with '/'"));
    }
    if !path_prefix.ends_with('/') {
        path_prefix.push('/');
    }
    if path_prefix.len() > MAX_PACKAGE_PATH_BYTES {
        return Err(refused("a registry path is too long"));
    }
    if path_prefix
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(refused("a registry path must not contain traversal"));
    }
    Ok(PackageOrigin {
        scheme,
        host: host.to_owned(),
        port,
        path_prefix,
    })
}

impl ControlStore {
    /// Every recorded package-network profile, in the order they were added.
    pub fn package_profiles(&self) -> &[PackageProfileStatus] {
        &self.snapshot.package_profiles
    }

    /// The profile with this name, compared case-insensitively.
    pub fn package_profile(&self, name: &str) -> Option<&PackageProfileStatus> {
        self.snapshot
            .package_profiles
            .iter()
            .find(|status| status.profile.name.eq_ignore_ascii_case(name))
    }

    /// Record a package-network profile. This advances the settings revision
    /// and records no runtime authority.
    pub fn add_package_profile(
        &mut self,
        declaration: &PackageProfileDeclaration,
        expected_revision: u64,
    ) -> Result<PackageProfile, ControlError> {
        self.check_revision(expected_revision)?;
        if self.package_profiles().len() >= MAX_PACKAGE_PROFILES {
            return Err(refused(
                "at most 32 package-network profiles may be recorded",
            ));
        }
        if self.package_profile(&declaration.name).is_some() {
            return Err(refused(
                "a package-network profile with this name already exists; remove it first",
            ));
        }
        let origins = declaration
            .origins
            .iter()
            .map(|origin| parse_origin(origin))
            .collect::<Result<Vec<_>, _>>()?;
        let profile = PackageProfile {
            id: Uuid::new_v4(),
            name: declaration.name.clone(),
            origins,
        };
        validate_profile(&profile)?;
        self.append(
            Action::PackageProfileAdded {
                profile: profile.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(profile)
    }

    /// Remove a package-network profile by name. This advances the settings
    /// revision.
    pub fn remove_package_profile(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Uuid, ControlError> {
        self.check_revision(expected_revision)?;
        let profile_id = self
            .package_profile(name)
            .ok_or(ControlError::UnknownPackageProfile)?
            .profile
            .id;
        self.append(
            Action::PackageProfileRemoved { profile_id },
            self.next_revision()?,
        )?;
        Ok(profile_id)
    }
}

/// Validate and apply one package-profile action during replay or append
/// preflight.
pub(super) fn apply(
    state: &mut ControlSnapshot,
    action: &Action,
    revision: u64,
) -> Result<(), ControlError> {
    match action {
        Action::PackageProfileAdded { profile } => {
            validate_profile(profile)
                .map_err(|_| ControlError::Invalid("invalid package profile"))?;
            if state.package_profiles.len() >= MAX_PACKAGE_PROFILES
                || state.package_profiles.iter().any(|status| {
                    status.profile.id == profile.id
                        || status.profile.name.eq_ignore_ascii_case(&profile.name)
                })
            {
                return Err(ControlError::Invalid(
                    "duplicate package profile or too many package profiles",
                ));
            }
            state.package_profiles.push(PackageProfileStatus {
                profile: profile.clone(),
                revision,
            });
        }
        Action::PackageProfileRemoved { profile_id } => {
            let index = state
                .package_profiles
                .iter()
                .position(|status| status.profile.id == *profile_id)
                .ok_or(ControlError::Invalid(
                    "removal of an unknown package profile",
                ))?;
            state.package_profiles.remove(index);
        }
        _ => {}
    }
    Ok(())
}

/// Check everything about a profile that needs no network or filesystem.
fn validate_profile(profile: &PackageProfile) -> Result<(), ControlError> {
    if !valid_uuid(profile.id) || !valid_profile_name(&profile.name) {
        return Err(refused(
            "package-network profile names are 1 to 64 ASCII letters, digits, '.', '-', or '_', starting with a letter or digit",
        ));
    }
    if profile.origins.is_empty() || profile.origins.len() > MAX_PACKAGE_ORIGINS {
        return Err(refused(
            "a package-network profile needs 1 to 8 registry origins",
        ));
    }
    for (index, origin) in profile.origins.iter().enumerate() {
        validate_origin(origin)?;
        if profile.origins[..index]
            .iter()
            .any(|earlier| same_origin(earlier, origin))
        {
            return Err(refused("a registry origin is listed twice"));
        }
    }
    Ok(())
}

/// Check stored origin fields, including values that replay could otherwise
/// accept without a URL parse.
fn validate_origin(origin: &PackageOrigin) -> Result<(), ControlError> {
    if origin.host.is_empty()
        || origin.host.len() > MAX_PACKAGE_HOST_BYTES
        || !origin.host.is_ascii()
        || origin.host.ends_with('.')
        || origin
            .host
            .chars()
            .any(|character| character.is_ascii_control() || character.is_whitespace())
    {
        return Err(refused("a registry host is invalid"));
    }
    if origin.scheme == PackageScheme::Http && !is_loopback_host(&origin.host) {
        return Err(refused("http is allowed only for a loopback test fixture"));
    }
    if origin.port == 0 {
        return Err(refused("a registry origin needs a nonzero port"));
    }
    let path = &origin.path_prefix;
    if path.len() > MAX_PACKAGE_PATH_BYTES
        || !path.starts_with('/')
        || !path.ends_with('/')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(refused("a registry path prefix is invalid"));
    }
    Ok(())
}

fn same_origin(first: &PackageOrigin, second: &PackageOrigin) -> bool {
    first.scheme == second.scheme
        && first.host.eq_ignore_ascii_case(&second.host)
        && first.port == second.port
        && first.path_prefix == second.path_prefix
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host.eq_ignore_ascii_case("::1")
        || host.eq_ignore_ascii_case("[::1]")
}

fn refused(reason: &'static str) -> ControlError {
    ControlError::PackageProfileRefused(reason)
}

#[cfg(test)]
mod tests;

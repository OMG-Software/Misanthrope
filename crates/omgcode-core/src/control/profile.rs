//! Operator-declared toolchain profiles.
//!
//! A profile names the read-only toolchain roots, the `PATH` entries, and the
//! extra environment values that a trusted `run` may use. The operator
//! declares every value; nothing is discovered or probed. Each profile is
//! recorded in the control journal and changes the settings revision. A
//! profile is configuration, not a grant: `run` rechecks every directory
//! before it starts anything. See `docs/execution.md` and ADR-0030.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_uuid};
use crate::workspace::DirectoryRecord;

/// Most recorded profiles.
pub const MAX_PROFILES: usize = 32;
/// Most read-only roots in one profile.
pub const MAX_PROFILE_ROOTS: usize = 16;
/// Most `PATH` entries in one profile.
pub const MAX_PROFILE_PATH_ENTRIES: usize = 16;
/// Most extra environment variables in one profile.
pub const MAX_PROFILE_VARIABLES: usize = 64;
/// Longest variable name, in bytes.
pub const MAX_VARIABLE_NAME_BYTES: usize = 128;
/// Longest variable value, in bytes.
pub const MAX_VARIABLE_VALUE_BYTES: usize = 4096;
/// Longest profile name, in characters.
pub const MAX_PROFILE_NAME_CHARS: usize = 64;

/// Variables that `run` sets itself. A profile may not set them, in any case.
pub const RESERVED_VARIABLES: [&str; 11] = [
    "PATH",
    "HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "LOCALAPPDATA",
    "SYSTEMROOT",
    "WINDIR",
    "XDG_CACHE_HOME",
    "OMGCODE_PACKAGE_CACHE",
];

/// The native platform a profile was declared on. A profile runs only there.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePlatform {
    /// Linux.
    Linux,
    /// Windows.
    Windows,
}

impl ProfilePlatform {
    /// The platform of this build, or `None` where `run` is unsupported.
    pub const fn current() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(windows) {
            Some(Self::Windows)
        } else {
            None
        }
    }
}

/// One extra environment value. Values are stored in the private settings
/// journal and shown by `workspace status`; never put secrets in them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileVariable {
    name: String,
    value: String,
}

impl ProfileVariable {
    /// The variable name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The variable value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// One recorded profile. Paths keep their native encoding and stay private.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainProfile {
    id: Uuid,
    name: String,
    platform: ProfilePlatform,
    read_only_roots: Vec<DirectoryRecord>,
    path_entries: Vec<DirectoryRecord>,
    environment: Vec<ProfileVariable>,
}

impl ToolchainProfile {
    /// Stable profile identifier, recorded with each run.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The operator's name for the profile.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The platform the profile was declared on.
    pub const fn platform(&self) -> ProfilePlatform {
        self.platform
    }

    /// Extra environment values, in declaration order.
    pub fn environment(&self) -> &[ProfileVariable] {
        &self.environment
    }

    pub(crate) fn read_only_roots(&self) -> &[DirectoryRecord] {
        &self.read_only_roots
    }

    pub(crate) fn path_entries(&self) -> &[DirectoryRecord] {
        &self.path_entries
    }
}

/// A profile and the settings revision that recorded it.
#[derive(Clone, Debug, Serialize)]
pub struct ProfileStatus {
    #[serde(flatten)]
    profile: ToolchainProfile,
    revision: u64,
}

impl ProfileStatus {
    /// The recorded profile.
    pub const fn profile(&self) -> &ToolchainProfile {
        &self.profile
    }

    /// The settings revision of the event that added it.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

/// An operator's profile declaration, before any directory is checked.
#[derive(Clone, Debug, Default)]
pub struct ProfileDeclaration {
    /// The profile name: 1 to 64 ASCII letters, digits, `.`, `-`, or `_`,
    /// starting with a letter or digit. Names compare case-insensitively.
    pub name: String,
    /// Existing directories the run may read and execute. At least one.
    pub read_only_roots: Vec<PathBuf>,
    /// Existing directories, each inside a root, searched for bare program
    /// names and joined in order as the run's `PATH`.
    pub path_entries: Vec<PathBuf>,
    /// Extra environment values. Names must not be reserved.
    pub environment: Vec<(String, String)>,
}

impl ControlStore {
    /// Every recorded profile, in the order they were added.
    pub fn profiles(&self) -> &[ProfileStatus] {
        &self.snapshot.profiles
    }

    /// The profile with this name, compared case-insensitively.
    pub fn profile(&self, name: &str) -> Option<&ProfileStatus> {
        self.snapshot
            .profiles
            .iter()
            .find(|status| status.profile.name.eq_ignore_ascii_case(name))
    }

    /// Record a new profile after checking every directory.
    ///
    /// Each root must exist, must not overlap application storage, and must
    /// neither contain nor lie inside the development root. Each `PATH` entry
    /// must lie inside one of the roots. This advances the settings revision.
    pub fn add_profile(
        &mut self,
        declaration: &ProfileDeclaration,
        expected_revision: u64,
    ) -> Result<ToolchainProfile, ControlError> {
        self.check_revision(expected_revision)?;
        let platform = ProfilePlatform::current().ok_or(ControlError::ProfileRefused(
            "toolchain profiles exist only on Linux and Windows",
        ))?;
        if self.profile(&declaration.name).is_some() {
            return Err(ControlError::ProfileRefused(
                "a profile with this name already exists; remove it first",
            ));
        }
        let mut roots: Vec<DirectoryRecord> = Vec::new();
        for path in &declaration.read_only_roots {
            let root = DirectoryRecord::capture(path)?;
            self.check_toolchain_root(&root)?;
            if roots.iter().any(|other| other.same_directory(&root)) {
                return Err(ControlError::ProfileRefused("a root is listed twice"));
            }
            roots.push(root);
        }
        let mut path_entries = Vec::new();
        for path in &declaration.path_entries {
            let entry = DirectoryRecord::capture(path)?;
            if !Self::inside_any(&roots, &entry)? {
                return Err(ControlError::ProfileRefused(
                    "each PATH entry must lie inside a read-only root",
                ));
            }
            path_entries.push(entry);
        }
        let profile = ToolchainProfile {
            id: Uuid::new_v4(),
            name: declaration.name.clone(),
            platform,
            read_only_roots: roots,
            path_entries,
            environment: declaration
                .environment
                .iter()
                .map(|(name, value)| ProfileVariable {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
        };
        // Replay validation also runs in the append preflight; check the
        // shape first so the operator gets the specific reason.
        validate_shape(&profile)?;
        self.append(
            Action::ProfileAdded {
                profile: profile.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(profile)
    }

    /// Remove a profile by name. This advances the settings revision. Runs
    /// already started keep their recorded profile.
    pub fn remove_profile(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Uuid, ControlError> {
        self.check_revision(expected_revision)?;
        let profile_id = self
            .profile(name)
            .ok_or(ControlError::UnknownProfile)?
            .profile
            .id;
        self.append(Action::ProfileRemoved { profile_id }, self.next_revision()?)?;
        Ok(profile_id)
    }

    /// Recheck a toolchain root against storage and the development root.
    pub(crate) fn check_toolchain_root(&self, root: &DirectoryRecord) -> Result<(), ControlError> {
        self.check_storage_overlap(root)?;
        let development = self
            .snapshot
            .development_root
            .as_ref()
            .ok_or(ControlError::NoRoot)?;
        if development.contains(root)? || root.contains(development)? {
            return Err(ControlError::ProfileRefused(
                "toolchain roots must not overlap the development root",
            ));
        }
        Ok(())
    }

    fn inside_any(
        roots: &[DirectoryRecord],
        entry: &DirectoryRecord,
    ) -> Result<bool, ControlError> {
        for root in roots {
            if root.contains(entry)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Whether `name` is reserved for the runner, compared case-insensitively.
pub fn is_reserved_variable(name: &str) -> bool {
    RESERVED_VARIABLES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// Check a profile name: 1 to 64 ASCII letters, digits, `.`, `-`, or `_`,
/// starting with a letter or digit.
pub fn valid_profile_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && name.len() <= MAX_PROFILE_NAME_CHARS
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

/// A variable name: an ASCII letter or `_`, then letters, digits, or `_`.
fn valid_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name.len() <= MAX_VARIABLE_NAME_BYTES
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Check everything about a profile that needs no filesystem access.
fn validate_shape(profile: &ToolchainProfile) -> Result<(), ControlError> {
    if !valid_uuid(profile.id) || !valid_profile_name(&profile.name) {
        return Err(ControlError::ProfileRefused(
            "profile names are 1 to 64 ASCII letters, digits, '.', '-', or '_', starting with a letter or digit",
        ));
    }
    if profile.read_only_roots.is_empty() || profile.read_only_roots.len() > MAX_PROFILE_ROOTS {
        return Err(ControlError::ProfileRefused(
            "a profile needs 1 to 16 read-only roots",
        ));
    }
    if profile.path_entries.len() > MAX_PROFILE_PATH_ENTRIES {
        return Err(ControlError::ProfileRefused(
            "a profile has at most 16 PATH entries",
        ));
    }
    if profile.environment.len() > MAX_PROFILE_VARIABLES {
        return Err(ControlError::ProfileRefused(
            "a profile has at most 64 environment variables",
        ));
    }
    if !profile
        .read_only_roots
        .iter()
        .chain(&profile.path_entries)
        .all(DirectoryRecord::validate_shape)
    {
        return Err(ControlError::Invalid("invalid profile directory"));
    }
    for (index, variable) in profile.environment.iter().enumerate() {
        if !valid_variable_name(&variable.name) {
            return Err(ControlError::ProfileRefused(
                "variable names are ASCII letters, digits, and '_', not starting with a digit",
            ));
        }
        if is_reserved_variable(&variable.name) {
            return Err(ControlError::ProfileRefused(
                "this variable is set by the runner and cannot be declared",
            ));
        }
        if variable.value.len() > MAX_VARIABLE_VALUE_BYTES || variable.value.contains('\0') {
            return Err(ControlError::ProfileRefused(
                "variable values are at most 4096 bytes and contain no NUL",
            ));
        }
        if profile.environment[..index]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(&variable.name))
        {
            return Err(ControlError::ProfileRefused("a variable is declared twice"));
        }
    }
    Ok(())
}

/// Validate and apply one profile action during replay or append preflight.
pub(super) fn apply(
    state: &mut ControlSnapshot,
    action: &Action,
    revision: u64,
) -> Result<(), ControlError> {
    match action {
        Action::ProfileAdded { profile } => {
            validate_shape(profile).map_err(|_| ControlError::Invalid("invalid profile"))?;
            if state.profiles.len() >= MAX_PROFILES
                || state.profiles.iter().any(|status| {
                    status.profile.id == profile.id
                        || status.profile.name.eq_ignore_ascii_case(&profile.name)
                })
            {
                return Err(ControlError::Invalid(
                    "duplicate profile or too many profiles",
                ));
            }
            state.profiles.push(ProfileStatus {
                profile: profile.clone(),
                revision,
            });
        }
        Action::ProfileRemoved { profile_id } => {
            let index = state
                .profiles
                .iter()
                .position(|status| status.profile.id == *profile_id)
                .ok_or(ControlError::Invalid("removal of an unknown profile"))?;
            state.profiles.remove(index);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;

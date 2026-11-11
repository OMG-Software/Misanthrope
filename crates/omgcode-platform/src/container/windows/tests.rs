//! Grant and revoke round trips on real NTFS directories.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::broker::windows::tests::create_junction_bounded;
use crate::container::{ContainerAccess, ContainerName, grant as public_grant};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A unique test container whose profile is deleted on drop, so test runs do
/// not leave per-user AppContainer profiles behind.
pub(crate) struct TestContainer(ContainerName);

impl std::ops::Deref for TestContainer {
    type Target = ContainerName;

    fn deref(&self) -> &ContainerName {
        &self.0
    }
}

impl Drop for TestContainer {
    fn drop(&mut self) {
        if let Err(error) = delete_profile(&self.0) {
            eprintln!("could not delete test profile {}: {error}", self.0.as_str());
        }
    }
}

pub(crate) fn unique_name() -> TestContainer {
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    TestContainer(
        ContainerName::new(&format!("OMGCode.Test.{}.{count}", std::process::id()))
            .expect("test container name"),
    )
}

pub(crate) fn identity(path: &Path) -> DirectoryIdentity {
    let directory = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .expect("open directory");
    directory_identity(&directory).expect("directory identity")
}

fn access(path: &Path, name: &ContainerName) -> Option<ContainerAccess> {
    granted_access(path, &derive_sid(name).expect("sid")).expect("granted access")
}

fn protected(path: &Path) -> bool {
    ObjectSecurity::read(&open_object(path).expect("object"))
        .expect("security")
        .dacl_protected()
        .expect("protection")
}

#[test]
fn derived_sids_are_stable_container_sids() {
    let name = unique_name();
    let first = sid_string(&derive_sid(&name).expect("sid")).expect("text");
    let second = sid_string(&derive_sid(&name).expect("sid")).expect("text");
    let other = sid_string(&derive_sid(&unique_name()).expect("sid")).expect("text");
    assert!(first.starts_with("S-1-15-2-"), "{first}");
    assert_eq!(first, second);
    assert_ne!(first, other);
    assert_eq!(
        crate::container::container_sid(&name).expect("public sid"),
        first
    );
}

#[test]
fn the_first_grant_creates_a_profile_that_can_be_deleted() {
    let root = tempfile::tempdir().expect("root");
    let name = unique_name();
    delete_profile(&name).expect("deleting a missing profile succeeds");
    assert!(!profile_exists(&name).expect("lookup"));
    grant(
        root.path(),
        identity(root.path()),
        &name,
        ContainerAccess::ReadExecute,
    )
    .expect("grant");
    assert!(profile_exists(&name).expect("lookup"));
    assert!(!ensure_profile(&name).expect("existing profile is kept"));
    revoke(root.path(), identity(root.path()), &name).expect("revoke");
    assert!(profile_exists(&name).expect("revocation keeps the profile"));
    delete_profile(&name).expect("delete");
    assert!(!profile_exists(&name).expect("lookup"));
}

#[test]
fn grants_propagate_to_children_and_revocation_removes_them() {
    let root = tempfile::tempdir().expect("root");
    let granted = root.path().join("workspace");
    let nested = granted.join("nested");
    fs::create_dir_all(&nested).expect("tree");
    let file = nested.join("file.txt");
    fs::write(&file, b"data").expect("file");
    let name = unique_name();
    let expected = identity(&granted);

    assert_eq!(access(&granted, &name), None);
    assert_eq!(access(&file, &name), None);

    public_grant(&granted, expected, &name, ContainerAccess::ReadExecute).expect("read grant");
    for path in [&granted, &nested, &file] {
        assert_eq!(access(path, &name), Some(ContainerAccess::ReadExecute));
    }
    // A second grant replaces the entry instead of adding one.
    grant(&granted, expected, &name, ContainerAccess::ReadWrite).expect("write grant");
    for path in [&granted, &nested, &file] {
        assert_eq!(access(path, &name), Some(ContainerAccess::ReadWrite));
    }
    let sid = derive_sid(&name).expect("sid");
    let directory = open_object(&granted).expect("granted");
    assert_eq!(
        explicit_entries(&directory, &sid).expect("entries"),
        [Explicit::Allowed {
            flags: GRANT_INHERITANCE,
            mask: READ_WRITE_MASK
        }]
    );
    // New children inherit the entry without another grant.
    let later = granted.join("later.txt");
    fs::write(&later, b"later").expect("later file");
    assert_eq!(access(&later, &name), Some(ContainerAccess::ReadWrite));

    revoke(&granted, expected, &name).expect("revoke");
    for path in [&granted, &nested, &file, &later] {
        assert_eq!(access(path, &name), None, "{}", path.display());
    }
    assert!(
        explicit_entries(&directory, &sid)
            .expect("entries")
            .is_empty()
    );
    // Revoking again is a confirmed no-op.
    revoke(&granted, expected, &name).expect("second revoke");
    assert_eq!(fs::read(&file).expect("data unchanged"), b"data");
}

#[test]
fn grants_keep_the_protection_state_and_other_entries() {
    let root = tempfile::tempdir().expect("root");
    let granted = root.path().join("workspace");
    fs::create_dir(&granted).expect("directory");
    let before = protected(&granted);
    let user = crate::private_store::windows::current_user_sid().expect("user");
    let user_mask = allowed_mask(&open_object(&granted).expect("object"), &[&user]).expect("mask");
    let name = unique_name();
    grant(
        &granted,
        identity(&granted),
        &name,
        ContainerAccess::ReadWrite,
    )
    .expect("grant");
    assert_eq!(protected(&granted), before);
    assert_eq!(
        allowed_mask(&open_object(&granted).expect("object"), &[&user]).expect("mask"),
        user_mask
    );
    revoke(&granted, identity(&granted), &name).expect("revoke");
    assert_eq!(protected(&granted), before);
}

#[test]
fn propagation_does_not_follow_junctions_out_of_the_grant() {
    let root = tempfile::tempdir().expect("root");
    let granted = root.path().join("workspace");
    let outside = root.path().join("outside");
    fs::create_dir(&granted).expect("granted");
    fs::create_dir(&outside).expect("outside");
    let secret = outside.join("secret.txt");
    fs::write(&secret, b"secret").expect("secret");
    let link = granted.join("link");
    create_junction_bounded(&link, &outside).expect("junction");
    let name = unique_name();

    grant(
        &granted,
        identity(&granted),
        &name,
        ContainerAccess::ReadWrite,
    )
    .expect("grant");
    assert_eq!(access(&outside, &name), None);
    assert_eq!(access(&secret, &name), None);
    let error = granted_access(&link, &derive_sid(&name).expect("sid"))
        .expect_err("a junction is refused as a confinement path");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    revoke(&granted, identity(&granted), &name).expect("revoke");
}

#[test]
fn hard_links_carry_a_grant_to_their_other_names() {
    // A hard link shares one security descriptor, so propagation reaches the
    // file under every name. This records the documented limit; it is why
    // same-user actors are not isolated from grants.
    let root = tempfile::tempdir().expect("root");
    let granted = root.path().join("workspace");
    let outside = root.path().join("outside");
    fs::create_dir(&granted).expect("granted");
    fs::create_dir(&outside).expect("outside");
    let original = outside.join("shared.txt");
    fs::write(&original, b"shared").expect("original");
    fs::hard_link(&original, granted.join("linked.txt")).expect("hard link");
    let name = unique_name();

    grant(
        &granted,
        identity(&granted),
        &name,
        ContainerAccess::ReadExecute,
    )
    .expect("grant");
    assert_eq!(access(&original, &name), Some(ContainerAccess::ReadExecute));
    revoke(&granted, identity(&granted), &name).expect("revoke");
}

#[test]
fn grants_refuse_a_replaced_or_unsuitable_directory() {
    let root = tempfile::tempdir().expect("root");
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::create_dir(&first).expect("first");
    fs::create_dir(&second).expect("second");
    let file = root.path().join("file.txt");
    fs::write(&file, b"x").expect("file");
    let link = root.path().join("link");
    create_junction_bounded(&link, &first).expect("junction");
    let name = unique_name();
    let expected = identity(&first);

    let replaced =
        grant(&second, expected, &name, ContainerAccess::ReadWrite).expect_err("another directory");
    assert_eq!(replaced.kind(), io::ErrorKind::InvalidInput);
    grant(&link, expected, &name, ContainerAccess::ReadWrite).expect_err("a junction");
    grant(&file, expected, &name, ContainerAccess::ReadWrite).expect_err("a file");
    grant(
        Path::new("relative"),
        expected,
        &name,
        ContainerAccess::ReadWrite,
    )
    .expect_err("a relative path");
    revoke(&second, expected, &name).expect_err("revoke checks identity too");
    assert_eq!(access(&first, &name), None);
    assert_eq!(access(&second, &name), None);
}

#[test]
fn all_packages_entries_count_toward_access() {
    let windows = std::env::var_os("SystemRoot").expect("SystemRoot");
    let system = Path::new(&windows).join("System32");
    assert_eq!(
        access(&system, &unique_name()),
        Some(ContainerAccess::ReadExecute),
        "System32 is readable by every application package"
    );
}

use super::*;
use std::fs;
use std::path::PathBuf;

use crate::control::{ControlError, ControlStore, FILE_NAME};

fn store() -> (tempfile::TempDir, PathBuf, ControlStore) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("projects");
    fs::create_dir_all(root.join("app")).expect("development root");
    let mut store = ControlStore::open(&data).expect("store");
    store.set_development_root(&root, 0).expect("root");
    (directory, data, store)
}

fn declaration(name: &str, origins: &[&str]) -> PackageProfileDeclaration {
    PackageProfileDeclaration {
        name: name.into(),
        origins: origins.iter().map(|origin| (*origin).to_owned()).collect(),
    }
}

/// A profile is recorded with its revision, survives replay, and is removed.
#[test]
fn package_profiles_are_recorded_replayed_and_removed() {
    let (_directory, data, mut store) = store();
    let profile = store
        .add_package_profile(
            &declaration(
                "crates",
                &[
                    "https://registry.example.com/api/v4",
                    "http://127.0.0.1:8080/repository/",
                ],
            ),
            1,
        )
        .expect("profile");
    assert_eq!(store.revision(), 2);
    let status = store
        .package_profile("CRATES")
        .expect("case-insensitive lookup");
    assert_eq!(status.profile().id(), profile.id());
    assert_eq!(status.revision(), 2);
    assert_eq!(status.profile().origins()[0].path_prefix(), "/api/v4/");
    assert_eq!(status.profile().origins()[0].port(), 443);
    assert_eq!(status.profile().origins()[1].scheme(), PackageScheme::Http);
    assert_eq!(status.profile().origins()[1].port(), 8080);
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot JSON");
    assert_eq!(encoded["package_profiles"][0]["name"], "crates");
    assert_eq!(encoded["package_profiles"][0]["revision"], 2);
    drop(store);

    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.package_profiles().len(), 1);
    assert!(matches!(
        store.add_package_profile(&declaration("Crates", &["https://other.example.com/"]), 2),
        Err(ControlError::PackageProfileRefused(_))
    ));
    assert_eq!(
        store.remove_package_profile("crates", 2).expect("remove"),
        profile.id()
    );
    assert_eq!(store.revision(), 3);
    assert!(store.package_profiles().is_empty());
    assert!(matches!(
        store.remove_package_profile("crates", 3),
        Err(ControlError::UnknownPackageProfile)
    ));
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot JSON");
    assert!(encoded.get("package_profiles").is_none());
    drop(store);
    assert!(
        ControlStore::open(&data)
            .expect("replay")
            .package_profiles()
            .is_empty()
    );
}

/// Origins are normalized, and only https (or loopback http) is accepted.
#[test]
fn origins_are_normalized_and_restricted() {
    let origin = parse_origin("https://registry.example.com").expect("default path");
    assert_eq!(origin.scheme(), PackageScheme::Https);
    assert_eq!(origin.host(), "registry.example.com");
    assert_eq!(origin.port(), 443);
    assert_eq!(origin.path_prefix(), "/");

    let origin = parse_origin("https://registry.example.com/api/v4").expect("path prefix");
    assert_eq!(origin.path_prefix(), "/api/v4/");

    let origin = parse_origin("http://localhost:8080/fixture/").expect("loopback http");
    assert_eq!(origin.scheme(), PackageScheme::Http);
    assert_eq!(origin.host(), "localhost");
    assert_eq!(origin.port(), 8080);

    let origin = parse_origin("http://[::1]:9000/").expect("IPv6 loopback");
    assert_eq!(origin.host(), "[::1]");
}

/// Invalid declarations are refused without an event or revision change.
#[test]
fn invalid_package_profiles_are_refused_without_a_record() {
    let (_directory, data, mut store) = store();
    let nine: Vec<String> = (0..9)
        .map(|index| format!("https://registry{index}.example.com/"))
        .collect();
    let cases = [
        ("no origins", declaration("none", &[])),
        (
            "nine origins",
            PackageProfileDeclaration {
                name: "nine".into(),
                origins: nine,
            },
        ),
        (
            "http outside loopback",
            declaration("plain", &["http://registry.example.com/"]),
        ),
        (
            "credentials",
            declaration("credentials", &["https://user@registry.example.com/"]),
        ),
        (
            "query",
            declaration("query", &["https://registry.example.com/?q=1"]),
        ),
        (
            "fragment",
            declaration("fragment", &["https://registry.example.com/#x"]),
        ),
        (
            "unsupported scheme",
            declaration("ftp", &["ftp://registry.example.com/"]),
        ),
        (
            "zero port",
            declaration("port", &["https://registry.example.com:0/"]),
        ),
        (
            "duplicate origin",
            declaration(
                "duplicate",
                &[
                    "https://registry.example.com/",
                    "https://REGISTRY.example.com/",
                ],
            ),
        ),
        (
            "bad name",
            declaration("bad name", &["https://registry.example.com/"]),
        ),
        (
            "empty name",
            declaration("", &["https://registry.example.com/"]),
        ),
    ];
    for (name, declaration) in cases {
        let result = store.add_package_profile(&declaration, 1);
        assert!(
            matches!(result, Err(ControlError::PackageProfileRefused(_))),
            "{name}: {result:?}"
        );
    }
    assert_eq!(store.revision(), 1);
    assert!(store.package_profiles().is_empty());
    drop(store);
    let text = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    assert!(!text.contains("package_profile_added"), "{text}");
}

/// Global profile and per-profile origin bounds are enforced.
#[test]
fn package_profile_bounds_are_enforced() {
    let (_directory, _data, mut store) = store();
    for index in 0..MAX_PACKAGE_PROFILES {
        let revision = store.revision();
        store
            .add_package_profile(
                &declaration(&format!("p{index}"), &["https://registry.example.com/"]),
                revision,
            )
            .expect("profile within the bound");
    }
    let revision = store.revision();
    assert!(matches!(
        store.add_package_profile(
            &declaration("extra", &["https://registry.example.com/"]),
            revision
        ),
        Err(ControlError::PackageProfileRefused(_))
    ));

    let revision = store.revision();
    store
        .remove_package_profile("p0", revision)
        .expect("free a profile slot");
    let host = format!("{}{}", "a".repeat(200), "b".repeat(52));
    let path = format!("/{}/", "c".repeat(1000));
    let origins: Vec<String> = (0..MAX_PACKAGE_ORIGINS)
        .map(|index| format!("https://{index}{host}{path}"))
        .collect();
    let revision = store.revision();
    store
        .add_package_profile(
            &PackageProfileDeclaration {
                name: "largest".into(),
                origins,
            },
            revision,
        )
        .expect("a full profile fits one event");
}

/// Replay rejects a profile whose stored origin would fail the declaration
/// rules, even though it was not parsed from a URL this time.
#[test]
fn replay_refuses_a_tampered_package_profile() {
    let (_directory, data, mut store) = store();
    store
        .add_package_profile(
            &declaration("crates", &["https://registry.example.com/"]),
            1,
        )
        .expect("profile");
    drop(store);
    let journal = data.join(FILE_NAME);
    let text = fs::read_to_string(&journal).expect("journal text");
    let tampered = text.replacen("\"scheme\":\"https\"", "\"scheme\":\"http\"", 1);
    assert_ne!(text, tampered);
    fs::write(&journal, tampered).expect("tamper journal");
    assert!(matches!(
        ControlStore::open(&data),
        Err(ControlError::Invalid(_))
    ));
}

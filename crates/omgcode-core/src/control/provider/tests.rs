use super::*;
use std::fs;

use crate::control::{ControlError, ControlStore, FILE_NAME};

fn store() -> (tempfile::TempDir, std::path::PathBuf, ControlStore) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let store = ControlStore::open(&data).expect("store");
    (directory, data, store)
}

fn declaration(name: &str, kind: &str, auth: &str) -> ProviderProfileDeclaration {
    ProviderProfileDeclaration {
        name: name.into(),
        kind: kind.into(),
        auth: auth.into(),
    }
}

/// Profiles are recorded with revisions, survive replay, and are removed.
#[test]
fn provider_profiles_are_recorded_replayed_and_removed() {
    let (_directory, data, mut store) = store();
    let profile = store
        .add_provider_profile(&declaration("work", "openai", "subscription"), 0)
        .expect("profile");
    assert_eq!(store.revision(), 1);
    let status = store.provider_profile("WORK").expect("case-insensitive");
    assert_eq!(status.profile().id(), profile.id());
    assert_eq!(status.profile().kind(), ProviderKind::OpenAi);
    assert_eq!(status.profile().auth(), ProviderAuth::Subscription);
    assert_eq!(status.revision(), 1);
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert_eq!(encoded["provider_profiles"][0]["name"], "work");
    assert_eq!(encoded["provider_profiles"][0]["kind"], "openai");
    assert_eq!(encoded["provider_profiles"][0]["auth"], "subscription");
    drop(store);

    let mut store = ControlStore::open(&data).expect("replay");
    assert_eq!(store.provider_profiles().len(), 1);
    assert!(matches!(
        store.add_provider_profile(&declaration("WORK", "anthropic", "api_key"), 1),
        Err(ControlError::ProviderRefused(_))
    ));
    assert_eq!(
        store.remove_provider_profile("work", 1).expect("remove"),
        profile.id()
    );
    assert_eq!(store.revision(), 2);
    assert!(store.provider_profiles().is_empty());
    assert!(matches!(
        store.remove_provider_profile("work", 2),
        Err(ControlError::UnknownProvider)
    ));
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot");
    assert!(encoded.get("provider_profiles").is_none());
}

/// A provider and authentication pair must be compatible, and a profile
/// cannot be duplicated or use an unknown spelling.
#[test]
fn invalid_provider_profiles_are_refused_without_a_record() {
    let (_directory, data, mut store) = store();
    let cases = [
        ("openai", "none"),
        ("anthropic", "none"),
        ("ollama_local", "api_key"),
        ("ollama_cloud", "subscription"),
        ("open_router", "subscription"),
        ("lm_studio", "subscription"),
        ("unknown", "none"),
        ("openai", "password"),
    ];
    for (kind, auth) in cases {
        let result = store.add_provider_profile(&declaration("bad", kind, auth), 0);
        assert!(
            matches!(result, Err(ControlError::ProviderRefused(_))),
            "{kind}/{auth}: {result:?}"
        );
    }
    assert_eq!(store.revision(), 0);
    assert!(store.provider_profiles().is_empty());

    store
        .add_provider_profile(&declaration("work", "openai", "api_key"), 0)
        .expect("profile");
    let result = store.add_provider_profile(&declaration("work", "anthropic", "api_key"), 1);
    assert!(matches!(result, Err(ControlError::ProviderRefused(_))));
    drop(store);
    let text = fs::read_to_string(data.join(FILE_NAME)).expect("journal text");
    assert_eq!(text.matches("provider_profile_added").count(), 1, "{text}");
}

/// Local providers use `none`; cloud key providers require an API key.
#[test]
fn provider_authentication_pairs_are_supported() {
    let (_directory, _data, mut store) = store();
    for (index, (kind, auth)) in [
        ("openai", "subscription"),
        ("anthropic", "api_key"),
        ("ollama_local", "none"),
        ("ollama_cloud", "api_key"),
        ("open_router", "api_key"),
        ("lm_studio", "none"),
        ("lm_studio", "api_key"),
    ]
    .into_iter()
    .enumerate()
    {
        let revision = store.revision();
        store
            .add_provider_profile(&declaration(&format!("p{index}"), kind, auth), revision)
            .unwrap_or_else(|error| panic!("{kind}/{auth}: {error}"));
    }
}

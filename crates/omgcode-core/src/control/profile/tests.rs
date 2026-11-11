use super::*;
use std::fs;
use std::path::Path;

use crate::control::{AuditEntry, FILE_NAME};

struct Scene {
    _directory: tempfile::TempDir,
    data: PathBuf,
    root: PathBuf,
    tools: PathBuf,
    store: ControlStore,
}

/// A store with a development root and a toolchain directory beside it.
fn scene() -> Scene {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let root = directory.path().join("projects");
    let tools = directory.path().join("tools");
    fs::create_dir_all(root.join("app")).expect("development root");
    fs::create_dir_all(tools.join("bin")).expect("toolchain bin");
    let mut store = ControlStore::open(&data).expect("store");
    store.set_development_root(&root, 0).expect("root");
    Scene {
        _directory: directory,
        data,
        root,
        tools,
        store,
    }
}

fn declaration(scene: &Scene) -> ProfileDeclaration {
    ProfileDeclaration {
        name: "tools".into(),
        read_only_roots: vec![scene.tools.clone()],
        path_entries: vec![scene.tools.join("bin")],
        environment: vec![("TOOL_MODE".into(), "offline".into())],
    }
}

fn declaration_with(tools: &Path, name: &str) -> ProfileDeclaration {
    ProfileDeclaration {
        name: name.into(),
        read_only_roots: vec![tools.to_path_buf()],
        ..ProfileDeclaration::default()
    }
}

fn write_entries(data: &Path, entries: &[AuditEntry]) {
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend(serde_json::to_vec(entry).expect("event JSON"));
        bytes.push(b'\n');
    }
    fs::write(data.join(FILE_NAME), bytes).expect("journal fixture");
}

/// A profile is recorded with its revision, survives replay, and is removed.
#[test]
fn profiles_are_recorded_replayed_and_removed() {
    let mut scene = scene();
    let profile = scene
        .store
        .add_profile(&declaration(&scene), 1)
        .expect("profile");
    assert_eq!(scene.store.revision(), 2);
    let status = scene
        .store
        .profile("TOOLS")
        .expect("case-insensitive lookup");
    assert_eq!(status.profile().id(), profile.id());
    assert_eq!(status.revision(), 2);
    assert_eq!(status.profile().environment()[0].value(), "offline");
    let encoded = serde_json::to_value(scene.store.snapshot()).expect("snapshot JSON");
    assert_eq!(encoded["profiles"][0]["name"], "tools");
    assert_eq!(encoded["profiles"][0]["revision"], 2);
    drop(scene.store);

    let mut store = ControlStore::open(&scene.data).expect("replay");
    assert_eq!(store.profiles().len(), 1);
    assert!(matches!(
        store.add_profile(&declaration_with(&scene.tools, "Tools"), 2),
        Err(ControlError::ProfileRefused(_))
    ));
    assert_eq!(
        store.remove_profile("tools", 2).expect("remove"),
        profile.id()
    );
    assert_eq!(store.revision(), 3);
    assert!(store.profiles().is_empty());
    assert!(matches!(
        store.remove_profile("tools", 3),
        Err(ControlError::UnknownProfile)
    ));
    let encoded = serde_json::to_value(store.snapshot()).expect("snapshot JSON");
    assert!(encoded.get("profiles").is_none());
    drop(store);
    assert!(
        ControlStore::open(&scene.data)
            .expect("replay")
            .profiles()
            .is_empty()
    );
}

/// Roots that overlap the development root or storage, PATH entries outside
/// the roots, and bad names or variables are refused without a record.
#[test]
fn invalid_declarations_are_refused_without_a_record() {
    let mut scene = scene();
    let outside = scene.tools.parent().expect("parent").join("elsewhere");
    fs::create_dir(&outside).expect("other directory");
    let parent = scene.root.parent().expect("parent").to_path_buf();
    let cases: Vec<(&str, ProfileDeclaration)> = vec![
        (
            "no roots",
            ProfileDeclaration {
                name: "x".into(),
                ..ProfileDeclaration::default()
            },
        ),
        (
            "inside development root",
            declaration_with(&scene.root.join("app"), "x"),
        ),
        (
            "development root itself",
            declaration_with(&scene.root, "x"),
        ),
        ("contains development root", declaration_with(&parent, "x")),
        ("storage", declaration_with(&scene.data, "x")),
        ("bad name", declaration_with(&scene.tools, ".hidden")),
        ("long name", declaration_with(&scene.tools, &"n".repeat(65))),
        (
            "path outside roots",
            ProfileDeclaration {
                path_entries: vec![outside.clone()],
                ..declaration_with(&scene.tools, "x")
            },
        ),
        (
            "duplicate root",
            ProfileDeclaration {
                read_only_roots: vec![scene.tools.clone(), scene.tools.join("bin").join("..")],
                ..declaration_with(&scene.tools, "x")
            },
        ),
        (
            "missing root",
            declaration_with(&scene.tools.join("gone"), "x"),
        ),
    ];
    let before = scene.store.audit(100).expect("audit").len();
    for (label, declaration) in cases {
        assert!(scene.store.add_profile(&declaration, 1).is_err(), "{label}");
    }
    let variables = [
        ("PATH", "x"),
        ("path", "x"),
        ("LocalAppData", "x"),
        ("1BAD", "x"),
        ("BAD-NAME", "x"),
        ("VALUE", "a\0b"),
    ];
    for (name, value) in variables {
        let declaration = ProfileDeclaration {
            environment: vec![(name.into(), value.into())],
            ..declaration_with(&scene.tools, "x")
        };
        assert!(
            matches!(
                scene.store.add_profile(&declaration, 1),
                Err(ControlError::ProfileRefused(_))
            ),
            "{name}"
        );
    }
    let duplicate = ProfileDeclaration {
        environment: vec![("A".into(), "1".into()), ("a".into(), "2".into())],
        ..declaration_with(&scene.tools, "x")
    };
    assert!(scene.store.add_profile(&duplicate, 1).is_err());
    let oversized = ProfileDeclaration {
        environment: vec![("A".into(), "v".repeat(MAX_VARIABLE_VALUE_BYTES + 1))],
        ..declaration_with(&scene.tools, "x")
    };
    assert!(scene.store.add_profile(&oversized, 1).is_err());
    assert!(matches!(
        scene.store.add_profile(&declaration(&scene), 0),
        Err(ControlError::Conflict { .. })
    ));
    assert_eq!(scene.store.audit(100).expect("audit").len(), before);
    assert_eq!(scene.store.revision(), 1);
}

/// Replay refuses records that the live checks would never write.
#[test]
fn replay_refuses_inconsistent_profile_records() {
    let mut scene = scene();
    scene
        .store
        .add_profile(&declaration(&scene), 1)
        .expect("profile");
    let entries = scene.store.entries.clone();
    drop(scene.store);
    let added = entries.last().expect("profile event").clone();
    let Action::ProfileAdded { profile } = &added.action else {
        panic!("expected a profile event");
    };
    type Mutation = Box<dyn Fn(&mut ToolchainProfile)>;
    let mutations: Vec<(&str, Mutation)> = vec![
        ("name", Box::new(|profile| profile.name = "bad name".into())),
        (
            "no roots",
            Box::new(|profile| profile.read_only_roots.clear()),
        ),
        (
            "reserved variable",
            Box::new(|profile| profile.environment[0].name = "Path".into()),
        ),
        ("nil id", Box::new(|profile| profile.id = Uuid::nil())),
    ];
    for (label, mutate) in mutations {
        let mut changed = profile.clone();
        mutate(&mut changed);
        let mut tampered = entries.clone();
        if let Some(last) = tampered.last_mut() {
            last.action = Action::ProfileAdded { profile: changed };
        }
        write_entries(&scene.data, &tampered);
        assert!(ControlStore::open(&scene.data).is_err(), "{label}");
    }
    let mut duplicated = entries.clone();
    let mut second = added.clone();
    second.event_id = Uuid::new_v4();
    second.sequence += 1;
    second.revision += 1;
    duplicated.push(second);
    write_entries(&scene.data, &duplicated);
    assert!(ControlStore::open(&scene.data).is_err(), "duplicate");
    let mut unknown = entries;
    let mut removal = added;
    removal.event_id = Uuid::new_v4();
    removal.sequence += 1;
    removal.revision += 1;
    removal.action = Action::ProfileRemoved {
        profile_id: Uuid::new_v4(),
    };
    unknown.push(removal);
    write_entries(&scene.data, &unknown);
    assert!(ControlStore::open(&scene.data).is_err(), "unknown removal");
}

#[test]
fn reserved_names_and_profile_names_are_checked_case_insensitively() {
    for name in RESERVED_VARIABLES {
        assert!(is_reserved_variable(name));
        assert!(is_reserved_variable(&name.to_ascii_lowercase()));
    }
    assert!(!is_reserved_variable("CARGO_HOME"));
    assert!(valid_profile_name("rust-1.95_stable.x"));
    assert!(!valid_profile_name(""));
    assert!(!valid_profile_name("-x"));
    assert!(!valid_profile_name("a b"));
}

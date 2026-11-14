use super::*;
use std::ffi::OsString;

fn store_id() -> Uuid {
    Uuid::new_v4()
}

fn start(run_id: Uuid) -> RunStart {
    RunStart {
        run_id,
        workspace_id: WorkspaceId::fresh(),
        settings_revision: 7,
        profile_id: Uuid::new_v4(),
        profile_name: "tools".into(),
        profile_revision: 3,
        program: NativePath::from_path(&std::env::temp_dir().join("tool")).expect("native path"),
        argument_count: 2,
        arguments_sha256: arguments_digest(&[OsString::from("a"), OsString::from("b")]),
        working_directory: NativePath::from_path(&std::env::temp_dir()).expect("native path"),
        unix_sockets: SocketMode::PairsOnly,
        limits: Some(LimitsRecord {
            max_memory_bytes: 8 << 30,
            max_processes: 1024,
            max_cpu_ms: 7_200_000,
            max_disk_bytes: Some(8 << 30),
        }),
        network: None,
        package_cache: false,
    }
}

fn finished(result: RunResult) -> RunFinish {
    RunFinish {
        result,
        confinement: (result != RunResult::NotStarted).then_some(ConfinementRecord::AppContainer),
        elapsed_ms: 12,
        stdout_bytes: 3,
        stderr_bytes: 0,
        usage: None,
    }
}

/// Tree limits, a limit outcome, and usage replay. A start recorded before
/// 0.17.0, with no limits, still replays. A run that did not start cannot
/// report usage, and recorded limits cannot be zero.
#[test]
fn limits_and_usage_are_recorded_and_checked_on_replay() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = store_id();
    let (limited, earlier) = (Uuid::new_v4(), Uuid::new_v4());
    let mut journal = RunJournal::open(directory.path(), store).expect("journal");
    journal.start(start(limited)).expect("start");
    let mut old_start = start(earlier);
    old_start.limits = None;
    journal.start(old_start).expect("start without limits");
    let mut outcome = finished(RunResult::LimitExceeded {
        limit: LimitKind::Memory,
    });
    outcome.usage = Some(UsageRecord {
        cpu_ms: 40,
        peak_memory_bytes: Some(1 << 27),
        peak_disk_bytes: Some(1 << 24),
    });
    journal.finish(limited, outcome).expect("finish");
    drop(journal);
    let text = fs::read_to_string(directory.path().join(FILE_NAME)).expect("journal text");
    assert!(text.contains(r#""kind":"limit_exceeded","limit":"memory""#));
    assert!(text.contains(r#""max_cpu_ms":7200000"#));
    assert!(text.contains(r#""max_disk_bytes":8589934592"#));
    assert!(text.contains(r#""peak_disk_bytes":16777216"#));
    // The earlier start's line carries no limits field at all.
    assert_eq!(text.matches("\"limits\"").count(), 1);
    let journal = RunJournal::open(directory.path(), store).expect("replay");
    assert_eq!(
        journal.outcome(limited),
        Some(RunResult::LimitExceeded {
            limit: LimitKind::Memory
        })
    );
    drop(journal);

    let mut state = ReplayState::default();
    let mut not_started = finished(RunResult::NotStarted);
    not_started.usage = Some(UsageRecord {
        cpu_ms: 0,
        peak_memory_bytes: None,
        peak_disk_bytes: None,
    });
    let run = Uuid::new_v4();
    let entries = [
        RunAction::RunStarted { start: start(run) },
        RunAction::RunFinished {
            run_id: run,
            finish: not_started,
        },
    ];
    let mut zero = start(Uuid::new_v4());
    zero.limits = Some(LimitsRecord {
        max_memory_bytes: 0,
        max_processes: 1,
        max_cpu_ms: 1,
        max_disk_bytes: Some(1),
    });
    assert!(matches!(
        RunJournal::apply(
            &mut state.clone(),
            &entry(store, 1, RunAction::RunStarted { start: zero }),
            1,
            store
        ),
        Err(RunJournalError::Invalid(_))
    ));
    RunJournal::apply(&mut state, &entry(store, 1, entries[0].clone()), 1, store).expect("start");
    assert!(matches!(
        RunJournal::apply(&mut state, &entry(store, 2, entries[1].clone()), 2, store),
        Err(RunJournalError::Invalid(_))
    ));
}

/// A networked start records its support paths. A start recorded before
/// 0.18.0, with no network field, still replays as a run without network
/// access, and a start cannot record more support paths than the bound.
#[test]
fn network_access_is_recorded_and_bounded_on_replay() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = store_id();
    let (networked, plain) = (Uuid::new_v4(), Uuid::new_v4());
    let support = NativePath::from_path(&std::env::temp_dir().join("hosts")).expect("native path");
    let mut journal = RunJournal::open(directory.path(), store).expect("journal");
    let mut networked_start = start(networked);
    networked_start.network = Some(NetworkRecord {
        support_paths: vec![support.clone()],
    });
    journal
        .start(networked_start.clone())
        .expect("networked start");
    journal.start(start(plain)).expect("plain start");
    drop(journal);
    let text = fs::read_to_string(directory.path().join(FILE_NAME)).expect("journal text");
    // Only the networked start's line carries the field.
    assert_eq!(text.matches("\"network\"").count(), 1);
    assert!(text.contains("\"support_paths\""));
    let journal = RunJournal::open(directory.path(), store).expect("replay");
    let replayed = journal.pending_starts();
    let found = |id| replayed.iter().find(|start| start.run_id == id).copied();
    assert_eq!(
        found(networked).and_then(|start| start.network.clone()),
        networked_start.network
    );
    assert_eq!(found(plain).map(|start| start.network.clone()), Some(None));
    drop(journal);

    let mut excessive = start(Uuid::new_v4());
    excessive.network = Some(NetworkRecord {
        support_paths: vec![support; MAX_NETWORK_SUPPORT_PATHS + 1],
    });
    assert!(matches!(
        RunJournal::apply(
            &mut ReplayState::default(),
            &entry(store, 1, RunAction::RunStarted { start: excessive }),
            1,
            store
        ),
        Err(RunJournalError::Invalid(_))
    ));
}

/// Intent and outcome pairs replay; several runs can be in flight at once.
#[test]
fn runs_are_recorded_and_replayed_with_several_in_flight() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = store_id();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    {
        let mut journal = RunJournal::open(directory.path(), store).expect("journal");
        journal.start(start(first)).expect("first start");
        journal.start(start(second)).expect("second start");
        assert_eq!(journal.pending().len(), 2);
        journal
            .finish(second, finished(RunResult::Exited { code: 0 }))
            .expect("second finish");
    }
    let mut journal = RunJournal::open(directory.path(), store).expect("reopen");
    assert_eq!(
        journal.pending(),
        &[PendingRun {
            run_id: first,
            state: PendingRunState::InProgress,
        }]
    );
    journal
        .finish(first, finished(RunResult::Cancelled))
        .expect("first finish after reopen");
    assert!(journal.pending().is_empty());
    assert!(matches!(
        journal.finish(first, finished(RunResult::Cancelled)),
        Err(RunJournalError::WrongRun(_))
    ));
    let entries = describe(journal.entries(100).expect("entries")).expect("JSON");
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[1]["action"]["type"], "run_started");
    assert_eq!(
        entries[1]["action"]["payload"]["start"]["unix_sockets"],
        "pairs_only"
    );
    assert_eq!(
        entries[4]["action"]["payload"]["finish"]["result"]["kind"],
        "cancelled"
    );
    assert!(journal.entries(0).is_err());
    assert!(journal.entries(101).is_err());
    drop(journal);
    assert!(matches!(
        RunJournal::open(directory.path(), store_id()),
        Err(RunJournalError::Invalid(_))
    ));
}

/// Unconfirmed cleanup stays pending until acknowledged; an acknowledged run
/// cannot be finished later; outcomes must name confinement consistently.
#[test]
fn unconfirmed_cleanup_needs_acknowledgement() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut journal = RunJournal::open(directory.path(), store_id()).expect("journal");
    let run = Uuid::new_v4();
    journal.start(start(run)).expect("start");
    journal
        .finish(run, finished(RunResult::CleanupUnconfirmed))
        .expect("finish");
    assert_eq!(
        journal.pending()[0].state,
        PendingRunState::NeedsAcknowledgement
    );
    assert!(journal.finish(run, finished(RunResult::Cancelled)).is_err());
    journal.acknowledge(run).expect("acknowledge");
    assert!(journal.pending().is_empty());
    assert!(journal.acknowledge(run).is_err());

    let abandoned = Uuid::new_v4();
    journal.start(start(abandoned)).expect("start");
    journal
        .acknowledge(abandoned)
        .expect("acknowledge in progress");
    assert!(matches!(
        journal.finish(abandoned, finished(RunResult::Exited { code: 0 })),
        Err(RunJournalError::WrongRun(_))
    ));

    let inconsistent = Uuid::new_v4();
    journal.start(start(inconsistent)).expect("start");
    let mut not_started = finished(RunResult::NotStarted);
    not_started.confinement = Some(ConfinementRecord::Landlock { abi: 3 });
    assert!(journal.finish(inconsistent, not_started).is_err());
    let mut started = finished(RunResult::Exited { code: 1 });
    started.confinement = None;
    assert!(journal.finish(inconsistent, started).is_err());
    journal
        .finish(inconsistent, finished(RunResult::NotStarted))
        .expect("consistent outcome");
}

/// At most sixteen runs can be pending.
#[test]
fn pending_runs_are_bounded() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut journal = RunJournal::open(directory.path(), store_id()).expect("journal");
    for _ in 0..MAX_PENDING_RUNS {
        journal.start(start(Uuid::new_v4())).expect("start");
    }
    assert!(matches!(
        journal.start(start(Uuid::new_v4())),
        Err(RunJournalError::TooManyPending)
    ));
}

/// A start must leave room for every pending outcome; outcomes may use it.
#[test]
fn starts_keep_room_for_pending_outcomes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut journal = RunJournal::open(directory.path(), store_id()).expect("journal");
    let run = Uuid::new_v4();
    journal.start(start(run)).expect("start");
    // One pending run reserves two events. Fill the log so that a second
    // start would need 1 + 2 + 2 more events than remain.
    let filler = journal.entries[0].clone();
    journal.entries.resize(MAX_EVENTS - 4, filler);
    assert!(matches!(
        journal.start(start(Uuid::new_v4())),
        Err(RunJournalError::LimitReached)
    ));
    journal
        .finish(run, finished(RunResult::CleanupUnconfirmed))
        .expect("outcome uses the reserve");
    journal
        .acknowledge(run)
        .expect("acknowledgement uses the reserve");

    let directory = tempfile::tempdir().expect("temporary directory");
    let mut journal = RunJournal::open(directory.path(), store_id()).expect("journal");
    journal.bytes = MAX_BYTES - 2 * OUTCOME_EVENT_RESERVE_BYTES;
    assert!(matches!(
        journal.start(start(Uuid::new_v4())),
        Err(RunJournalError::LimitReached)
    ));
}

/// One envelope for replay checks.
fn entry(store: Uuid, sequence: u64, action: RunAction) -> RunEntry {
    RunEntry {
        schema_version: SCHEMA_VERSION,
        store_id: store,
        event_id: Uuid::new_v4(),
        sequence,
        timestamp: "2026-11-12T00:00:00Z".into(),
        action,
    }
}

/// A start or outcome written before 0.19.0 has no disk field. It must
/// deserialize with the default, and a 0.19.0 record must carry the field.
#[test]
fn disk_fields_are_optional_for_pre_0_19_records() {
    let mut earlier = start(Uuid::new_v4());
    let limits = earlier.limits.expect("limits");
    earlier.limits = Some(LimitsRecord {
        max_disk_bytes: None,
        ..limits
    });
    let text = serde_json::to_string(&earlier).expect("serialize start");
    assert!(!text.contains("max_disk_bytes"), "{text}");
    let decoded: RunStart = serde_json::from_str(&text).expect("deserialize start");
    assert_eq!(decoded, earlier);

    let usage = UsageRecord {
        cpu_ms: 1,
        peak_memory_bytes: None,
        peak_disk_bytes: None,
    };
    let text = serde_json::to_string(&usage).expect("serialize usage");
    assert!(!text.contains("peak_disk_bytes"), "{text}");
    let decoded: UsageRecord = serde_json::from_str(&text).expect("deserialize usage");
    assert_eq!(decoded, usage);
}

/// The largest outcome and acknowledgement fit the reserved budget.
#[test]
fn outcome_events_fit_their_reserve() {
    let finish = RunFinish {
        result: RunResult::Exited { code: i64::MIN },
        confinement: Some(ConfinementRecord::Landlock { abi: u32::MAX }),
        elapsed_ms: u64::MAX,
        stdout_bytes: u64::MAX,
        stderr_bytes: u64::MAX,
        usage: Some(UsageRecord {
            cpu_ms: u64::MAX,
            peak_memory_bytes: Some(u64::MAX),
            peak_disk_bytes: Some(u64::MAX),
        }),
    };
    for action in [
        RunAction::RunFinished {
            run_id: Uuid::new_v4(),
            finish,
        },
        RunAction::RunAcknowledged {
            run_id: Uuid::new_v4(),
        },
    ] {
        let entry = RunEntry {
            schema_version: SCHEMA_VERSION,
            store_id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            sequence: u64::MAX,
            timestamp: "9999-12-31T23:59:59.999999999Z".into(),
            action,
        };
        assert!(encode(&entry).expect("encode").len() <= OUTCOME_EVENT_RESERVE_BYTES);
    }
}

/// Archiving refuses pending runs, keeps the exact bytes, and lets a new
/// journal start.
#[test]
fn archive_moves_a_settled_journal() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let archive = directory.path().join("archive");
    fs::create_dir(&archive).expect("archive directory");
    let store = store_id();
    let run = Uuid::new_v4();
    {
        let mut journal = RunJournal::open(&data, store).expect("journal");
        journal.start(start(run)).expect("start");
    }
    assert!(matches!(
        RunJournal::archive(&data, store, &archive),
        Err(RunJournalError::Pending(id)) if id == run
    ));
    {
        let mut journal = RunJournal::open(&data, store).expect("journal");
        journal
            .finish(run, finished(RunResult::Exited { code: 0 }))
            .expect("finish");
    }
    let bytes = fs::read(data.join(FILE_NAME)).expect("journal bytes");
    let archived = RunJournal::archive(&data, store, &archive).expect("archive");
    assert_eq!(fs::read(&archived.path).expect("archived bytes"), bytes);
    assert_eq!(archived.sha256, hex(&Sha256::digest(&bytes)));
    assert_eq!(archived.event_count, 3);
    assert!(!data.join(FILE_NAME).exists());
    let inspected = RunJournal::open_archive(&archived.path, store).expect("inspect");
    assert_eq!(inspected.entries(100).expect("entries").len(), 3);
    let mut inspected = inspected;
    assert!(inspected.start(start(Uuid::new_v4())).is_err());
    let fresh = RunJournal::open(&data, store).expect("new journal");
    assert_eq!(fresh.entries(100).expect("entries").len(), 1);
}

/// An uncertain move or sync reports the candidate path.
#[test]
fn archive_reports_uncertain_moves() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    let archive = directory.path().join("archive");
    fs::create_dir(&archive).expect("archive directory");
    let store = store_id();
    drop(RunJournal::open(&data, store).expect("journal"));
    let result = RunJournal::archive_with(
        &data,
        store,
        &archive,
        |_, _| Err(io::Error::other("interrupted move")),
        |_| Ok(()),
    );
    assert!(matches!(
        result,
        Err(RunJournalError::ArchiveUncertain { .. })
    ));
    let result = RunJournal::archive_with(
        &data,
        store,
        &archive,
        |_, _| Err(io::Error::new(io::ErrorKind::AlreadyExists, "exists")),
        |_| Ok(()),
    );
    assert!(matches!(result, Err(RunJournalError::Io(_))));
}

/// Replay refuses tampered or inconsistent journals and changes nothing.
#[test]
fn replay_refuses_tampered_journals() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = store_id();
    let run = Uuid::new_v4();
    let entries = {
        let mut journal = RunJournal::open(directory.path(), store).expect("journal");
        journal.start(start(run)).expect("start");
        journal
            .finish(run, finished(RunResult::Exited { code: 0 }))
            .expect("finish");
        journal.entries.clone()
    };
    let path = directory.path().join(FILE_NAME);
    let write = |entries: &[RunEntry]| {
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend(encode(entry).expect("encode"));
        }
        fs::write(&path, &bytes).expect("fixture");
        bytes
    };
    type Tamper = Box<dyn Fn(&mut Vec<RunEntry>)>;
    let cases: Vec<(&str, Tamper)> = vec![
        (
            "sequence",
            Box::new(|entries: &mut Vec<RunEntry>| entries[1].sequence = 5),
        ),
        (
            "store",
            Box::new(|entries: &mut Vec<RunEntry>| entries[1].store_id = Uuid::new_v4()),
        ),
        (
            "duplicate event",
            Box::new(|entries: &mut Vec<RunEntry>| entries[2].event_id = entries[1].event_id),
        ),
        (
            "finish without start",
            Box::new(|entries: &mut Vec<RunEntry>| {
                entries.remove(1);
                entries[1].sequence = 1;
            }),
        ),
        (
            "repeated run",
            Box::new(|entries: &mut Vec<RunEntry>| {
                let mut again = entries[1].clone();
                again.event_id = Uuid::new_v4();
                again.sequence = 3;
                entries.push(again);
            }),
        ),
        (
            "bad digest",
            Box::new(|entries: &mut Vec<RunEntry>| {
                if let RunAction::RunStarted { start } = &mut entries[1].action {
                    start.arguments_sha256 = "XYZ".into();
                }
            }),
        ),
        (
            "created later",
            Box::new(|entries: &mut Vec<RunEntry>| entries[1].action = RunAction::Created),
        ),
    ];
    for (label, mutate) in cases {
        let mut tampered = entries.clone();
        mutate(&mut tampered);
        let bytes = write(&tampered);
        assert!(
            RunJournal::open(directory.path(), store).is_err(),
            "{label}"
        );
        assert_eq!(fs::read(&path).expect("unchanged"), bytes, "{label}");
    }
    fs::write(&path, b"").expect("empty journal");
    assert!(RunJournal::open(directory.path(), store).is_err());
}

/// The digest separates argument boundaries and never needs the text.
#[test]
fn argument_digest_keeps_boundaries() {
    let joined = arguments_digest(&[OsString::from("ab")]);
    let split = arguments_digest(&[OsString::from("a"), OsString::from("b")]);
    assert_ne!(joined, split);
    assert_eq!(
        split,
        arguments_digest(&[OsString::from("a"), OsString::from("b")])
    );
    assert_ne!(arguments_digest(&[]), arguments_digest(&[OsString::new()]));
    assert!(valid_digest(&joined));
}

/// A second writer waits for the first to release the lock.
#[test]
fn a_second_writer_waits_for_the_lock() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = store_id();
    let journal = RunJournal::open(directory.path(), store).expect("journal");
    let path = directory.path().to_path_buf();
    let (busy, contended) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        RunJournal::open_observed(&path, store, || {
            let _ = busy.send(());
        })
        .map(|_| ())
    });
    // The waiter reports a refused attempt while the first writer holds the
    // lock, so the test cannot pass without real contention.
    contended
        .recv_timeout(Duration::from_secs(10))
        .expect("the waiter met the held lock");
    assert!(!waiter.is_finished(), "the waiter opened a held journal");
    drop(journal);
    waiter
        .join()
        .expect("waiter thread")
        .expect("the waiter opens after the release");
}

/// Pending starts include runs whose cleanup was unconfirmed, and only a
/// recorded finish counts as an outcome. Acknowledgement records none.
#[test]
fn pending_starts_and_outcomes_follow_the_recorded_events() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut journal = RunJournal::open(directory.path(), store_id()).expect("journal");
    let (going, ended, unconfirmed, abandoned) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    for run in [going, ended, unconfirmed, abandoned] {
        journal.start(start(run)).expect("start");
    }
    journal
        .finish(ended, finished(RunResult::Revoked))
        .expect("finish");
    journal
        .finish(unconfirmed, finished(RunResult::CleanupUnconfirmed))
        .expect("finish");
    journal.acknowledge(abandoned).expect("acknowledge");
    let listed: Vec<_> = journal
        .pending_starts()
        .iter()
        .map(|start| start.run_id)
        .collect();
    assert_eq!(listed, vec![going, unconfirmed]);
    assert_eq!(journal.outcome(going), None);
    assert_eq!(journal.outcome(ended), Some(RunResult::Revoked));
    assert_eq!(
        journal.outcome(unconfirmed),
        Some(RunResult::CleanupUnconfirmed)
    );
    assert_eq!(journal.outcome(abandoned), None);
}

/// A package-cache exposure is recorded only when it applies; a run without
/// it keeps the earlier journal shape.
#[test]
fn package_cache_access_is_recorded_only_when_present() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = store_id();
    let (cached, plain) = (Uuid::new_v4(), Uuid::new_v4());
    let mut cached_start = start(cached);
    cached_start.package_cache = true;
    let mut journal = RunJournal::open(directory.path(), store).expect("journal");
    journal.start(cached_start).expect("cached start");
    journal.start(start(plain)).expect("plain start");
    drop(journal);
    let text = fs::read_to_string(directory.path().join(FILE_NAME)).expect("journal text");
    assert_eq!(text.matches("\"package_cache\"").count(), 1, "{text}");
    assert!(text.contains("\"package_cache\":true"), "{text}");
    let journal = RunJournal::open(directory.path(), store).expect("replay");
    let starts = journal.pending_starts();
    assert!(
        starts
            .iter()
            .any(|start| start.run_id == cached && start.package_cache)
    );
    assert!(
        starts
            .iter()
            .any(|start| start.run_id == plain && !start.package_cache)
    );
}

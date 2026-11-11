//! Shared registered-workspace state for tests that drive checkpointed file commands.

use std::fs;
use std::path::PathBuf;

use omgcode_core::control::ControlStore;
use omgcode_core::policy::{PermissionSet, WorkspaceId};
use tempfile::TempDir;
use uuid::Uuid;

use super::spawn_gate::without_child_spawns;

/// Owns all files used by CLI subprocesses so each test starts from isolated state.
pub(crate) struct Fixture {
    pub(crate) _temporary: TempDir,
    pub(crate) data: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) session_id: Uuid,
}

impl Fixture {
    /// Creates settings, a read/write registered workspace, and `note.txt`.
    pub(crate) fn new(original: &[u8]) -> Self {
        let temporary = tempfile::tempdir().expect("private fixture root");
        let data = temporary.path().join("application-data");
        let development = temporary.path().join("development");
        let workspace = development.join("project");
        fs::create_dir_all(&workspace).expect("workspace directories");
        fs::write(workspace.join("note.txt"), original).expect("original workspace file");
        #[cfg(windows)]
        restrict_to_current_user(&data);

        let workspace_id = without_child_spawns(|| {
            let mut control = ControlStore::open(&data).expect("private control store");
            control
                .set_development_root(&development, control.revision())
                .expect("development root");
            let workspace_id = control
                .register_workspace(&workspace, control.revision())
                .expect("registered workspace")
                .id();
            control
                .set_permissions(
                    workspace_id,
                    PermissionSet {
                        read: true,
                        write: true,
                        execute: false,
                        task_network: false,
                        model_tools: false,
                    },
                    control.revision(),
                )
                .expect("explicit file preferences");
            workspace_id
        });

        Self {
            _temporary: temporary,
            data,
            workspace,
            workspace_id,
            session_id: Uuid::new_v4(),
        }
    }
}

/// Gives `data` the private ACL the Windows store requires of its root.
///
/// Temporary directories inherit grants that the store correctly refuses.
#[cfg(windows)]
fn restrict_to_current_user(data: &std::path::Path) {
    use std::process::Command;

    use super::{CLI_DEADLINE, run_with_deadline};

    let run = |command: Command| {
        let result = run_with_deadline(command, CLI_DEADLINE).expect("run ACL fixture command");
        assert!(!result.timed_out, "ACL fixture command timed out");
        assert!(result.output.status.success(), "ACL fixture command failed");
        String::from_utf8(result.output.stdout).expect("UTF-8 output")
    };
    let mut whoami = Command::new("whoami");
    whoami.args(["/user", "/fo", "csv", "/nh"]);
    let identity = run(whoami);
    let sid = identity
        .trim()
        .rsplit(',')
        .next()
        .expect("SID column")
        .trim_matches('"')
        .to_owned();
    fs::create_dir(data).expect("application data");
    let mut icacls = Command::new("icacls");
    icacls
        .arg(data)
        .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")]);
    run(icacls);
}

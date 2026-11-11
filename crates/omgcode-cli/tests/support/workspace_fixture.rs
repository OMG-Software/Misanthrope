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

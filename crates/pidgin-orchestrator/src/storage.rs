//! JSON-file persistence mirroring `packages/orchestrator/src/storage.ts`.
//!
//! The machine record lives in `machine.json` and the instance list in
//! `instances.json`, both under the orchestrator directory. Writes use
//! 2-space-indented JSON (matching pi's `JSON.stringify(value, null, 2)`) and
//! ensure the directory exists first. Reads return the empty/absent case when
//! the file is missing, and surface parse failures as errors (pi throws from
//! `JSON.parse`).

use std::fs;
use std::io;
use std::path::Path;

use crate::config::{get_instances_path, get_machine_path, get_orchestrator_dir};
use crate::types::{InstanceRecord, MachineRecord};

/// Create the orchestrator directory if it does not yet exist
/// (`storage.ts:ensureOrchestratorDir`).
fn ensure_orchestrator_dir() -> io::Result<()> {
    let orchestrator_dir = get_orchestrator_dir();
    if !orchestrator_dir.exists() {
        fs::create_dir_all(&orchestrator_dir)?;
    }
    Ok(())
}

/// Serialize a value as pi does: 2-space-indented JSON, no trailing newline.
fn to_pretty_json<T: serde::Serialize + ?Sized>(value: &T) -> io::Result<String> {
    serde_json::to_string_pretty(value).map_err(to_invalid_data)
}

/// Parse JSON, surfacing decode failures as `InvalidData` I/O errors.
fn parse_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let data = fs::read_to_string(path)?;
    serde_json::from_str(&data).map_err(to_invalid_data)
}

fn to_invalid_data(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Load the machine record, or `None` when `machine.json` is absent
/// (`storage.ts:loadMachine`).
pub fn load_machine() -> io::Result<Option<MachineRecord>> {
    let machine_path = get_machine_path();
    if !machine_path.exists() {
        return Ok(None);
    }
    parse_json(&machine_path).map(Some)
}

/// Persist the machine record (`storage.ts:saveMachine`).
pub fn save_machine(machine: &MachineRecord) -> io::Result<()> {
    ensure_orchestrator_dir()?;
    fs::write(get_machine_path(), to_pretty_json(machine)?)
}

/// Remove `machine.json` if present (`storage.ts:deleteMachine`).
pub fn delete_machine() -> io::Result<()> {
    let machine_path = get_machine_path();
    if !machine_path.exists() {
        return Ok(());
    }
    fs::remove_file(machine_path)
}

/// Load all instance records, or an empty list when `instances.json` is absent
/// (`storage.ts:loadInstances`).
pub fn load_instances() -> io::Result<Vec<InstanceRecord>> {
    let instances_path = get_instances_path();
    if !instances_path.exists() {
        return Ok(Vec::new());
    }
    parse_json(&instances_path)
}

/// Persist the full instance list (`storage.ts:saveInstances`).
pub fn save_instances(instances: &[InstanceRecord]) -> io::Result<()> {
    ensure_orchestrator_dir()?;
    fs::write(get_instances_path(), to_pretty_json(instances)?)
}

/// Look up an instance by id (`storage.ts:getInstance`).
pub fn get_instance(instance_id: &str) -> io::Result<Option<InstanceRecord>> {
    Ok(load_instances()?
        .into_iter()
        .find(|instance| instance.id == instance_id))
}

/// Insert or replace an instance by id (`storage.ts:upsertInstance`).
pub fn upsert_instance(instance: InstanceRecord) -> io::Result<()> {
    let mut instances = load_instances()?;
    match instances
        .iter()
        .position(|existing| existing.id == instance.id)
    {
        Some(index) => instances[index] = instance,
        None => instances.push(instance),
    }
    save_instances(&instances)
}

/// Drop an instance by id (`storage.ts:removeInstance`).
pub fn remove_instance(instance_id: &str) -> io::Result<()> {
    let instances: Vec<InstanceRecord> = load_instances()?
        .into_iter()
        .filter(|instance| instance.id != instance_id)
        .collect();
    save_instances(&instances)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InstanceStatus;
    use std::sync::MutexGuard;

    struct StorageEnv {
        _lock: MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        saved: Option<String>,
    }

    impl StorageEnv {
        fn new() -> Self {
            let lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = std::env::var("PI_ORCHESTRATOR_DIR").ok();
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("PI_ORCHESTRATOR_DIR", dir.path());
            StorageEnv {
                _lock: lock,
                _dir: dir,
                saved,
            }
        }
    }

    impl Drop for StorageEnv {
        fn drop(&mut self) {
            match &self.saved {
                Some(value) => std::env::set_var("PI_ORCHESTRATOR_DIR", value),
                None => std::env::remove_var("PI_ORCHESTRATOR_DIR"),
            }
        }
    }

    fn sample_machine() -> MachineRecord {
        MachineRecord {
            id: "machine-1".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: Some("2026-01-02T00:00:00.000Z".to_string()),
            label: None,
        }
    }

    fn sample_instance(id: &str, status: InstanceStatus) -> InstanceRecord {
        InstanceRecord {
            id: id.to_string(),
            status,
            cwd: "/home/user/project".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_seen_at: None,
            label: None,
            session_id: None,
            session_file: None,
            radius_pi_id: None,
        }
    }

    #[test]
    fn load_machine_absent_returns_none() {
        let _env = StorageEnv::new();
        assert_eq!(load_machine().unwrap(), None);
    }

    #[test]
    fn machine_round_trips_through_disk() {
        let _env = StorageEnv::new();
        let machine = sample_machine();
        save_machine(&machine).unwrap();
        assert_eq!(load_machine().unwrap(), Some(machine));
    }

    #[test]
    fn save_machine_writes_two_space_indented_json() {
        let _env = StorageEnv::new();
        save_machine(&sample_machine()).unwrap();
        let on_disk = fs::read_to_string(get_machine_path()).unwrap();
        assert!(on_disk.contains("\n  \"id\": \"machine-1\""));
        assert!(!on_disk.ends_with('\n'));
    }

    #[test]
    fn delete_machine_is_idempotent_and_removes_file() {
        let _env = StorageEnv::new();
        delete_machine().unwrap();
        save_machine(&sample_machine()).unwrap();
        assert!(get_machine_path().exists());
        delete_machine().unwrap();
        assert!(!get_machine_path().exists());
        assert_eq!(load_machine().unwrap(), None);
    }

    #[test]
    fn load_instances_absent_returns_empty() {
        let _env = StorageEnv::new();
        assert_eq!(load_instances().unwrap(), Vec::new());
    }

    #[test]
    fn instances_round_trip_through_disk() {
        let _env = StorageEnv::new();
        let instances = vec![
            sample_instance("a", InstanceStatus::Online),
            sample_instance("b", InstanceStatus::Starting),
        ];
        save_instances(&instances).unwrap();
        assert_eq!(load_instances().unwrap(), instances);
    }

    #[test]
    fn get_instance_finds_by_id() {
        let _env = StorageEnv::new();
        save_instances(&[
            sample_instance("a", InstanceStatus::Online),
            sample_instance("b", InstanceStatus::Stopped),
        ])
        .unwrap();
        assert_eq!(
            get_instance("b").unwrap(),
            Some(sample_instance("b", InstanceStatus::Stopped))
        );
        assert_eq!(get_instance("missing").unwrap(), None);
    }

    #[test]
    fn upsert_inserts_then_replaces() {
        let _env = StorageEnv::new();
        upsert_instance(sample_instance("a", InstanceStatus::Starting)).unwrap();
        upsert_instance(sample_instance("b", InstanceStatus::Online)).unwrap();
        assert_eq!(load_instances().unwrap().len(), 2);

        // Replacing "a" updates in place without changing length or order.
        let mut updated = sample_instance("a", InstanceStatus::Error);
        updated.label = Some("failed".to_string());
        upsert_instance(updated.clone()).unwrap();

        let instances = load_instances().unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0], updated);
        assert_eq!(instances[1], sample_instance("b", InstanceStatus::Online));
    }

    #[test]
    fn remove_instance_drops_matching_id() {
        let _env = StorageEnv::new();
        save_instances(&[
            sample_instance("a", InstanceStatus::Online),
            sample_instance("b", InstanceStatus::Online),
        ])
        .unwrap();
        remove_instance("a").unwrap();
        let instances = load_instances().unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].id, "b");

        // Removing a missing id leaves the list unchanged.
        remove_instance("missing").unwrap();
        assert_eq!(load_instances().unwrap().len(), 1);
    }
}

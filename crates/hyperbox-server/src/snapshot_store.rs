use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;

use hyperbox_core::{
    ActiveSandboxRecord, AffinityRecord, HyperboxError, ProcessId, ProcessInfo, Result,
    SandboxConfig, SandboxId, SnapshotId, SnapshotMetadata, SnapshotStore,
};

#[derive(Debug, Clone, Default)]
pub struct InMemorySnapshotStore {
    snapshots: Arc<Mutex<HashMap<SnapshotId, SnapshotMetadata>>>,
    affinities: Arc<Mutex<HashMap<String, AffinityRecord>>>,
    active: Arc<Mutex<HashMap<SandboxId, ActiveSandboxRecord>>>,
    processes: Arc<Mutex<HashMap<ProcessId, ProcessInfo>>>,
}

#[async_trait::async_trait]
impl SnapshotStore for InMemorySnapshotStore {
    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
        note: Option<String>,
    ) -> Result<SnapshotMetadata> {
        let snapshot = SnapshotMetadata {
            id: SnapshotId::new(),
            sandbox_id: sandbox_id.clone(),
            affinity_name: config.affinity_name.clone(),
            template: config.template.clone(),
            config: config.clone(),
            created_at: Utc::now(),
            note,
        };

        self.snapshots
            .lock()
            .await
            .insert(snapshot.id.clone(), snapshot.clone());

        Ok(snapshot)
    }

    async fn get_snapshot(&self, snapshot_id: &SnapshotId) -> Result<Option<SnapshotMetadata>> {
        Ok(self.snapshots.lock().await.get(snapshot_id).cloned())
    }

    async fn list_for_template(&self, template: &str) -> Result<Vec<SnapshotMetadata>> {
        Ok(self
            .snapshots
            .lock()
            .await
            .values()
            .filter(|snapshot| snapshot.template == template)
            .cloned()
            .collect())
    }

    async fn bind_sandbox(&self, name: &str, sandbox_id: &SandboxId) -> Result<()> {
        let mut affinities = self.affinities.lock().await;
        let existing_snapshot = affinities
            .get(name)
            .and_then(|record| record.snapshot_id.clone());
        affinities.insert(
            name.to_string(),
            AffinityRecord {
                name: name.to_string(),
                sandbox_id: Some(sandbox_id.clone()),
                snapshot_id: existing_snapshot,
                updated_at: Utc::now(),
            },
        );
        Ok(())
    }

    async fn clear_sandbox_binding(&self, sandbox_id: &SandboxId) -> Result<()> {
        let mut affinities = self.affinities.lock().await;
        for record in affinities.values_mut() {
            if record.sandbox_id.as_ref() == Some(sandbox_id) {
                record.sandbox_id = None;
                record.updated_at = Utc::now();
            }
        }
        Ok(())
    }

    async fn set_affinity_snapshot(&self, name: &str, snapshot_id: &SnapshotId) -> Result<()> {
        let mut affinities = self.affinities.lock().await;
        let existing_sandbox = affinities
            .get(name)
            .and_then(|record| record.sandbox_id.clone());
        affinities.insert(
            name.to_string(),
            AffinityRecord {
                name: name.to_string(),
                sandbox_id: existing_sandbox,
                snapshot_id: Some(snapshot_id.clone()),
                updated_at: Utc::now(),
            },
        );
        Ok(())
    }

    async fn get_affinity(&self, name: &str) -> Result<Option<AffinityRecord>> {
        Ok(self.affinities.lock().await.get(name).cloned())
    }

    async fn upsert_active_sandbox(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        self.active.lock().await.insert(
            sandbox_id.clone(),
            ActiveSandboxRecord {
                sandbox_id: sandbox_id.clone(),
                config: config.clone(),
                created_at,
            },
        );
        Ok(())
    }

    async fn remove_active_sandbox(&self, sandbox_id: &SandboxId) -> Result<()> {
        self.active.lock().await.remove(sandbox_id);
        Ok(())
    }

    async fn list_active_sandboxes(&self) -> Result<Vec<ActiveSandboxRecord>> {
        Ok(self.active.lock().await.values().cloned().collect())
    }

    async fn upsert_process(&self, process: &ProcessInfo) -> Result<()> {
        self.processes
            .lock()
            .await
            .insert(process.id.clone(), process.clone());
        Ok(())
    }

    async fn get_process(&self, process_id: &ProcessId) -> Result<Option<ProcessInfo>> {
        Ok(self.processes.lock().await.get(process_id).cloned())
    }

    async fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        Ok(self.processes.lock().await.values().cloned().collect())
    }

    async fn remove_process(&self, process_id: &ProcessId) -> Result<()> {
        self.processes.lock().await.remove(process_id);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SqliteSnapshotStore {
    db_path: PathBuf,
}

impl SqliteSnapshotStore {
    pub fn open(db_path: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path).map_err(sql_err)?;
        Self::init_schema(&conn)?;
        Ok(Self { db_path })
    }

    pub fn open_default() -> Result<Self> {
        let from_env = std::env::var("HYPERBOX_STATE_DB").ok().map(PathBuf::from);
        if let Some(path) = from_env {
            return Self::open(path);
        }

        if let Ok(home) = std::env::var("HOME") {
            return Self::open(PathBuf::from(home).join(".hyperbox/state.db"));
        }

        Self::open(std::env::temp_dir().join("hyperbox/state.db"))
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS snapshots (
              snapshot_id TEXT PRIMARY KEY,
              sandbox_id TEXT NOT NULL,
              affinity_name TEXT,
              template TEXT NOT NULL,
              config_json TEXT NOT NULL,
              created_at TEXT NOT NULL,
              note TEXT
            );

            CREATE INDEX IF NOT EXISTS snapshots_template_idx
              ON snapshots(template);
            CREATE INDEX IF NOT EXISTS snapshots_affinity_idx
              ON snapshots(affinity_name);

            CREATE TABLE IF NOT EXISTS affinities (
              name TEXT PRIMARY KEY,
              sandbox_id TEXT,
              snapshot_id TEXT,
              updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS active_sandboxes (
              sandbox_id TEXT PRIMARY KEY,
              config_json TEXT NOT NULL,
              created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS processes (
              process_id TEXT PRIMARY KEY,
              sandbox_id TEXT NOT NULL,
              requested_sandbox_id TEXT,
              disposition TEXT NOT NULL,
              command_json TEXT NOT NULL,
              status TEXT NOT NULL,
              stdout_path TEXT NOT NULL,
              stderr_path TEXT NOT NULL,
              backend_pid INTEGER,
              exit_code INTEGER,
              started_at TEXT NOT NULL,
              finished_at TEXT,
              expires_at TEXT
            );

            CREATE INDEX IF NOT EXISTS processes_sandbox_idx
              ON processes(sandbox_id);
            CREATE INDEX IF NOT EXISTS processes_status_idx
              ON processes(status);
            ",
        )
        .map_err(sql_err)?;
        Ok(())
    }

    fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = Connection::open(&self.db_path).map_err(sql_err)?;
        Self::init_schema(&conn)?;
        f(&conn)
    }
}

#[async_trait::async_trait]
impl SnapshotStore for SqliteSnapshotStore {
    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
        note: Option<String>,
    ) -> Result<SnapshotMetadata> {
        self.with_conn(|conn| {
            let snapshot = SnapshotMetadata {
                id: SnapshotId::new(),
                sandbox_id: sandbox_id.clone(),
                affinity_name: config.affinity_name.clone(),
                template: config.template.clone(),
                config: config.clone(),
                created_at: Utc::now(),
                note,
            };

            conn.execute(
                "INSERT INTO snapshots (snapshot_id, sandbox_id, affinity_name, template, config_json, created_at, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    snapshot.id.0.to_string(),
                    snapshot.sandbox_id.0.to_string(),
                    snapshot.affinity_name,
                    snapshot.template,
                    serde_json::to_string(&snapshot.config)?,
                    snapshot.created_at.to_rfc3339(),
                    snapshot.note,
                ],
            )
            .map_err(sql_err)?;

            Ok(snapshot)
        })
    }

    async fn get_snapshot(&self, snapshot_id: &SnapshotId) -> Result<Option<SnapshotMetadata>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT snapshot_id, sandbox_id, affinity_name, template, config_json, created_at, note
                 FROM snapshots WHERE snapshot_id = ?1",
                params![snapshot_id.0.to_string()],
                |row| {
                    let snapshot_id: String = row.get(0)?;
                    let sandbox_id: String = row.get(1)?;
                    let affinity_name: Option<String> = row.get(2)?;
                    let template: String = row.get(3)?;
                    let config_json: String = row.get(4)?;
                    let created_at: String = row.get(5)?;
                    let note: Option<String> = row.get(6)?;

                    Ok(SnapshotMetadata {
                        id: SnapshotId(parse_uuid(&snapshot_id)?),
                        sandbox_id: SandboxId(parse_uuid(&sandbox_id)?),
                        affinity_name,
                        template,
                        config: serde_json::from_str::<SandboxConfig>(&config_json)
                            .map_err(to_sql_error)?,
                        created_at: parse_rfc3339_utc(&created_at).map_err(to_sql_error)?,
                        note,
                    })
                },
            )
            .optional()
            .map_err(sql_err)
        })
    }

    async fn list_for_template(&self, template: &str) -> Result<Vec<SnapshotMetadata>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                "SELECT snapshot_id, sandbox_id, affinity_name, template, config_json, created_at, note
                 FROM snapshots WHERE template = ?1 ORDER BY created_at DESC",
            )
                .map_err(sql_err)?;
            let rows = stmt.query_map(params![template], |row| {
                let snapshot_id: String = row.get(0)?;
                let sandbox_id: String = row.get(1)?;
                let affinity_name: Option<String> = row.get(2)?;
                let template: String = row.get(3)?;
                let config_json: String = row.get(4)?;
                let created_at: String = row.get(5)?;
                let note: Option<String> = row.get(6)?;

                Ok(SnapshotMetadata {
                    id: SnapshotId(parse_uuid(&snapshot_id)?),
                    sandbox_id: SandboxId(parse_uuid(&sandbox_id)?),
                    affinity_name,
                    template,
                    config: serde_json::from_str::<SandboxConfig>(&config_json)
                        .map_err(to_sql_error)?,
                    created_at: parse_rfc3339_utc(&created_at).map_err(to_sql_error)?,
                    note,
                })
            })
            .map_err(sql_err)?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_err)
        })
    }

    async fn bind_sandbox(&self, name: &str, sandbox_id: &SandboxId) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO affinities (name, sandbox_id, snapshot_id, updated_at)
                 VALUES (?1, ?2, NULL, ?3)
                 ON CONFLICT(name) DO UPDATE SET
                   sandbox_id = excluded.sandbox_id,
                   updated_at = excluded.updated_at",
                params![name, sandbox_id.0.to_string(), Utc::now().to_rfc3339()],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    async fn clear_sandbox_binding(&self, sandbox_id: &SandboxId) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE affinities SET sandbox_id = NULL, updated_at = ?2 WHERE sandbox_id = ?1",
                params![sandbox_id.0.to_string(), Utc::now().to_rfc3339()],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    async fn set_affinity_snapshot(&self, name: &str, snapshot_id: &SnapshotId) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO affinities (name, sandbox_id, snapshot_id, updated_at)
                 VALUES (?1, NULL, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET
                   snapshot_id = excluded.snapshot_id,
                   updated_at = excluded.updated_at",
                params![name, snapshot_id.0.to_string(), Utc::now().to_rfc3339()],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    async fn get_affinity(&self, name: &str) -> Result<Option<AffinityRecord>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT name, sandbox_id, snapshot_id, updated_at FROM affinities WHERE name = ?1",
                params![name],
                |row| {
                    let name: String = row.get(0)?;
                    let sandbox_id: Option<String> = row.get(1)?;
                    let snapshot_id: Option<String> = row.get(2)?;
                    let updated_at: String = row.get(3)?;

                    Ok(AffinityRecord {
                        name,
                        sandbox_id: sandbox_id
                            .map(|raw| parse_uuid(&raw).map(SandboxId))
                            .transpose()?,
                        snapshot_id: snapshot_id
                            .map(|raw| parse_uuid(&raw).map(SnapshotId))
                            .transpose()?,
                        updated_at: parse_rfc3339_utc(&updated_at).map_err(to_sql_error)?,
                    })
                },
            )
            .optional()
            .map_err(sql_err)
        })
    }

    async fn upsert_active_sandbox(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO active_sandboxes (sandbox_id, config_json, created_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(sandbox_id) DO UPDATE SET
                   config_json = excluded.config_json,
                   created_at = excluded.created_at",
                params![
                    sandbox_id.0.to_string(),
                    serde_json::to_string(config)?,
                    created_at.to_rfc3339(),
                ],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    async fn remove_active_sandbox(&self, sandbox_id: &SandboxId) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM active_sandboxes WHERE sandbox_id = ?1",
                params![sandbox_id.0.to_string()],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    async fn list_active_sandboxes(&self) -> Result<Vec<ActiveSandboxRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT sandbox_id, config_json, created_at
                     FROM active_sandboxes ORDER BY created_at ASC",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |row| {
                    let sandbox_id: String = row.get(0)?;
                    let config_json: String = row.get(1)?;
                    let created_at: String = row.get(2)?;

                    Ok(ActiveSandboxRecord {
                        sandbox_id: SandboxId(parse_uuid(&sandbox_id)?),
                        config: serde_json::from_str::<SandboxConfig>(&config_json)
                            .map_err(to_sql_error)?,
                        created_at: parse_rfc3339_utc(&created_at).map_err(to_sql_error)?,
                    })
                })
                .map_err(sql_err)?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_err)
        })
    }

    async fn upsert_process(&self, process: &ProcessInfo) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO processes (
                   process_id, sandbox_id, requested_sandbox_id, disposition, command_json,
                   status, stdout_path, stderr_path, backend_pid, exit_code, started_at,
                   finished_at, expires_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(process_id) DO UPDATE SET
                   sandbox_id = excluded.sandbox_id,
                   requested_sandbox_id = excluded.requested_sandbox_id,
                   disposition = excluded.disposition,
                   command_json = excluded.command_json,
                   status = excluded.status,
                   stdout_path = excluded.stdout_path,
                   stderr_path = excluded.stderr_path,
                   backend_pid = excluded.backend_pid,
                   exit_code = excluded.exit_code,
                   started_at = excluded.started_at,
                   finished_at = excluded.finished_at,
                   expires_at = excluded.expires_at",
                params![
                    process.id.0.to_string(),
                    process.sandbox_id.0.to_string(),
                    process
                        .requested_sandbox_id
                        .as_ref()
                        .map(|id| id.0.to_string()),
                    serde_json::to_string(&process.disposition)?,
                    serde_json::to_string(&process.command)?,
                    serde_json::to_string(&process.status)?,
                    process.stdout_path,
                    process.stderr_path,
                    process.backend_pid,
                    process.exit_code,
                    process.started_at.to_rfc3339(),
                    process.finished_at.map(|time| time.to_rfc3339()),
                    process.expires_at.map(|time| time.to_rfc3339()),
                ],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }

    async fn get_process(&self, process_id: &ProcessId) -> Result<Option<ProcessInfo>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT process_id, sandbox_id, requested_sandbox_id, disposition, command_json,
                        status, stdout_path, stderr_path, backend_pid, exit_code, started_at,
                        finished_at, expires_at
                 FROM processes
                 WHERE process_id = ?1",
                params![process_id.0.to_string()],
                |row| process_from_row(row),
            )
            .optional()
            .map_err(sql_err)
        })
    }

    async fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT process_id, sandbox_id, requested_sandbox_id, disposition, command_json,
                            status, stdout_path, stderr_path, backend_pid, exit_code, started_at,
                            finished_at, expires_at
                     FROM processes
                     ORDER BY started_at DESC",
                )
                .map_err(sql_err)?;
            let rows = stmt.query_map([], process_from_row).map_err(sql_err)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql_err)
        })
    }

    async fn remove_process(&self, process_id: &ProcessId) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM processes WHERE process_id = ?1",
                params![process_id.0.to_string()],
            )
            .map_err(sql_err)?;
            Ok(())
        })
    }
}

fn process_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessInfo> {
    let process_id: String = row.get(0)?;
    let sandbox_id: String = row.get(1)?;
    let requested_sandbox_id: Option<String> = row.get(2)?;
    let disposition_json: String = row.get(3)?;
    let command_json: String = row.get(4)?;
    let status_json: String = row.get(5)?;
    let stdout_path: String = row.get(6)?;
    let stderr_path: String = row.get(7)?;
    let backend_pid: Option<u32> = row.get(8)?;
    let exit_code: Option<i32> = row.get(9)?;
    let started_at: String = row.get(10)?;
    let finished_at: Option<String> = row.get(11)?;
    let expires_at: Option<String> = row.get(12)?;

    Ok(ProcessInfo {
        id: ProcessId(parse_uuid(&process_id)?),
        sandbox_id: SandboxId(parse_uuid(&sandbox_id)?),
        requested_sandbox_id: requested_sandbox_id
            .as_deref()
            .map(parse_uuid)
            .transpose()?
            .map(SandboxId),
        disposition: serde_json::from_str(&disposition_json).map_err(to_sql_error)?,
        command: serde_json::from_str(&command_json).map_err(to_sql_error)?,
        status: serde_json::from_str(&status_json).map_err(to_sql_error)?,
        stdout_path,
        stderr_path,
        backend_pid,
        exit_code,
        started_at: parse_rfc3339_utc(&started_at).map_err(to_sql_error)?,
        finished_at: finished_at
            .as_deref()
            .map(parse_rfc3339_utc)
            .transpose()
            .map_err(to_sql_error)?,
        expires_at: expires_at
            .as_deref()
            .map(parse_rfc3339_utc)
            .transpose()
            .map_err(to_sql_error)?,
    })
}

fn parse_uuid(raw: &str) -> rusqlite::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(raw).map_err(to_sql_error)
}

fn parse_rfc3339_utc(raw: &str) -> std::result::Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(raw).map(|v| v.with_timezone(&Utc))
}

fn to_sql_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}

fn sql_err(err: rusqlite::Error) -> HyperboxError {
    HyperboxError::ExecutionFailed(format!("sqlite state store error: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperbox_core::{ProcessDisposition, ProcessId, ProcessInfo, ProcessStatus};

    #[tokio::test]
    async fn snapshot_store_roundtrip() {
        let store = InMemorySnapshotStore::default();
        let sandbox_id = SandboxId::new();

        let created = store
            .create_snapshot(
                &sandbox_id,
                &SandboxConfig::default(),
                Some("warm start".to_string()),
            )
            .await
            .expect("create snapshot");

        let found = store
            .get_snapshot(&created.id)
            .await
            .expect("get snapshot")
            .expect("snapshot exists");

        assert_eq!(found.template, "python:3.12");
    }

    #[tokio::test]
    async fn sqlite_store_persists_affinity_and_snapshot() {
        let root = tempfile::tempdir().expect("tempdir");
        let db_path = root.path().join("state.db");
        let store = SqliteSnapshotStore::open(db_path).expect("open sqlite store");

        let sandbox_id = SandboxId::new();
        store
            .bind_sandbox("demo", &sandbox_id)
            .await
            .expect("bind sandbox");

        let mut config = SandboxConfig::default();
        config.affinity_name = Some("demo".to_string());
        let snapshot = store
            .create_snapshot(&sandbox_id, &config, Some("note".to_string()))
            .await
            .expect("create snapshot");

        store
            .set_affinity_snapshot("demo", &snapshot.id)
            .await
            .expect("set snapshot");

        let affinity = store
            .get_affinity("demo")
            .await
            .expect("get affinity")
            .expect("affinity exists");
        assert_eq!(affinity.sandbox_id, Some(sandbox_id));
        assert_eq!(affinity.snapshot_id, Some(snapshot.id));
    }

    #[tokio::test]
    async fn sqlite_store_persists_active_sandbox_records() {
        let root = tempfile::tempdir().expect("tempdir");
        let db_path = root.path().join("state.db");
        let store = SqliteSnapshotStore::open(db_path).expect("open sqlite store");

        let sandbox_id = SandboxId::new();
        let config = SandboxConfig::default();
        let created_at = Utc::now();
        store
            .upsert_active_sandbox(&sandbox_id, &config, created_at)
            .await
            .expect("upsert active sandbox");

        let listed = store
            .list_active_sandboxes()
            .await
            .expect("list active sandboxes");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].sandbox_id, sandbox_id);
        assert_eq!(listed[0].config.template, config.template);

        store
            .remove_active_sandbox(&listed[0].sandbox_id)
            .await
            .expect("remove active sandbox");
        assert!(
            store
                .list_active_sandboxes()
                .await
                .expect("list active sandboxes after delete")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn sqlite_store_persists_process_records() {
        let root = tempfile::tempdir().expect("tempdir");
        let db_path = root.path().join("state.db");
        let store = SqliteSnapshotStore::open(db_path).expect("open sqlite store");

        let record = ProcessInfo {
            id: ProcessId::new(),
            sandbox_id: SandboxId::new(),
            requested_sandbox_id: None,
            disposition: ProcessDisposition::CreatedNew,
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "echo ok".to_string(),
            ],
            status: ProcessStatus::Running,
            stdout_path: ".hyperbox/processes/stdout.log".to_string(),
            stderr_path: ".hyperbox/processes/stderr.log".to_string(),
            backend_pid: Some(42),
            exit_code: None,
            started_at: Utc::now(),
            finished_at: None,
            expires_at: None,
        };

        store.upsert_process(&record).await.expect("upsert process");

        let found = store
            .get_process(&record.id)
            .await
            .expect("get process")
            .expect("process exists");
        assert_eq!(found.sandbox_id, record.sandbox_id);
        assert_eq!(found.status, ProcessStatus::Running);

        let listed = store.list_processes().await.expect("list processes");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, record.id);

        store
            .remove_process(&record.id)
            .await
            .expect("remove process");
        assert!(
            store
                .list_processes()
                .await
                .expect("list processes after delete")
                .is_empty()
        );
    }
}

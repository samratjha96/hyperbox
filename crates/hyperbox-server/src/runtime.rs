use std::{collections::HashMap, path::PathBuf, sync::Arc};

use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, info, warn};

use hyperbox_core::{
    ExecOutcome, ExecRequest, FilePayload, HyperboxError, Result, SandboxBackend, SandboxConfig,
    SandboxId, SandboxInfo, SnapshotId, SnapshotMetadata, SnapshotStore, TemplateRegistry,
};

use crate::metrics::{MetricsCollector, MetricsSnapshot};

#[derive(Clone)]
pub struct HyperboxServer {
    backend: Arc<dyn SandboxBackend>,
    templates: TemplateRegistry,
    sandboxes: Arc<Mutex<HashMap<SandboxId, SandboxConfig>>>,
    metrics: MetricsCollector,
    snapshots: Arc<dyn SnapshotStore>,
    hydration_complete: Arc<OnceCell<()>>,
}

#[derive(Debug, Clone)]
pub struct ActiveSandboxInfo {
    pub info: SandboxInfo,
    pub affinity_name: Option<String>,
}

impl HyperboxServer {
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        if cfg!(test) {
            return Self::new_with_snapshots(
                backend,
                Arc::new(crate::InMemorySnapshotStore::default()),
            );
        }
        let snapshots: Arc<dyn SnapshotStore> = match crate::SqliteSnapshotStore::open_default() {
            Ok(store) => Arc::new(store),
            Err(err) => {
                warn!(error = %err, "failed to open sqlite snapshot store, falling back to in-memory");
                Arc::new(crate::InMemorySnapshotStore::default())
            }
        };
        Self::new_with_snapshots(backend, snapshots)
    }

    pub fn new_with_snapshots(
        backend: Arc<dyn SandboxBackend>,
        snapshots: Arc<dyn SnapshotStore>,
    ) -> Self {
        Self {
            backend,
            templates: TemplateRegistry::with_defaults(),
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
            metrics: MetricsCollector::default(),
            snapshots,
            hydration_complete: Arc::new(OnceCell::new()),
        }
    }

    pub fn templates(&self) -> Vec<String> {
        self.templates
            .list()
            .into_iter()
            .map(|template| template.name.clone())
            .collect()
    }

    async fn ensure_hydrated(&self) -> Result<()> {
        self.hydration_complete
            .get_or_try_init(|| async {
                let records = self.snapshots.list_active_sandboxes().await?;
                let mut retry_needed = false;
                for record in records {
                    let sandbox_id = record.sandbox_id.clone();
                    match self.backend.inspect(&sandbox_id).await {
                        Ok(_) => {
                            self.sandboxes
                                .lock()
                                .await
                                .insert(sandbox_id, record.config.clone());
                        }
                        Err(err) => {
                            if matches!(err, HyperboxError::SandboxNotFound(_)) {
                                warn!(
                                    sandbox_id = %record.sandbox_id.0,
                                    error = %err,
                                    "hydration could not verify sandbox; keeping persisted record"
                                );
                            } else {
                                retry_needed = true;
                                warn!(
                                    sandbox_id = %record.sandbox_id.0,
                                    error = %err,
                                    "hydration encountered transient failure; will retry"
                                );
                            }
                        }
                    }
                }
                if retry_needed {
                    return Err(HyperboxError::ExecutionFailed(
                        "hydration incomplete; retrying on next request".to_string(),
                    ));
                }
                Ok(())
            })
            .await
            .map(|_| ())
    }

    pub async fn create_sandbox(&self, config: SandboxConfig) -> Result<SandboxInfo> {
        self.ensure_hydrated().await?;
        self.templates.ensure_exists(&config.template)?;
        info!(
            template = %config.template,
            memory_mb = config.memory_mb,
            vcpu_count = config.vcpu_count,
            "runtime create_sandbox"
        );
        let lease = self.backend.create(config.clone()).await?;
        if let Some(name) = config.affinity_name.as_deref() {
            self.snapshots.bind_sandbox(name, &lease.id).await?;
        }
        self.snapshots
            .upsert_active_sandbox(&lease.id, &config, lease.info.created_at)
            .await?;
        self.sandboxes.lock().await.insert(lease.id.clone(), config);
        self.metrics.inc_create();
        info!(sandbox_id = %lease.id.0, template = %lease.info.template, "runtime sandbox created");
        Ok(lease.info)
    }

    pub async fn exec(&self, sandbox_id: &SandboxId, request: ExecRequest) -> Result<ExecOutcome> {
        self.ensure_hydrated().await?;
        debug!(
            sandbox_id = %sandbox_id.0,
            timeout_secs = request.timeout_secs,
            command = %request.command.join(" "),
            "runtime exec"
        );
        self.metrics.inc_exec();
        let outcome = self.backend.exec(sandbox_id, request).await;
        match &outcome {
            Ok(outcome) => {
                self.metrics.record_exec_latency(outcome.duration_ms).await;
                info!(
                    sandbox_id = %sandbox_id.0,
                    exit_code = outcome.exit_code,
                    duration_ms = outcome.duration_ms,
                    "runtime exec completed"
                );
            }
            Err(err) => {
                self.metrics.inc_exec_failure();
                warn!(sandbox_id = %sandbox_id.0, error = %err, "runtime exec failed");
            }
        }
        outcome
    }

    pub async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.ensure_hydrated().await?;
        self.backend.inspect(sandbox_id).await
    }

    pub async fn list_sandboxes(&self) -> Vec<ActiveSandboxInfo> {
        if let Err(err) = self.ensure_hydrated().await {
            warn!(error = %err, "runtime list_sandboxes proceeding without hydration");
        }
        let entries: Vec<(SandboxId, Option<String>)> = self
            .sandboxes
            .lock()
            .await
            .iter()
            .map(|(id, config)| (id.clone(), config.affinity_name.clone()))
            .collect();

        let mut rows = Vec::with_capacity(entries.len());
        for (sandbox_id, affinity_name) in entries {
            match self.backend.inspect(&sandbox_id).await {
                Ok(info) => rows.push(ActiveSandboxInfo {
                    info,
                    affinity_name,
                }),
                Err(err) => {
                    warn!(
                        sandbox_id = %sandbox_id.0,
                        error = %err,
                        "runtime list_sandboxes skipping missing sandbox"
                    );
                }
            }
        }
        rows.sort_by(|a, b| a.info.created_at.cmp(&b.info.created_at));
        rows
    }

    pub async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        self.ensure_hydrated().await?;
        self.backend.read_file(sandbox_id, path).await
    }

    pub async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()> {
        self.ensure_hydrated().await?;
        self.backend.write_file(sandbox_id, payload).await
    }

    pub async fn destroy_sandbox(&self, sandbox_id: &SandboxId) -> Result<()> {
        self.ensure_hydrated().await?;
        info!(sandbox_id = %sandbox_id.0, "runtime destroy_sandbox");
        self.backend.destroy(sandbox_id).await?;
        self.sandboxes.lock().await.remove(sandbox_id);
        self.snapshots.clear_sandbox_binding(sandbox_id).await?;
        self.snapshots.remove_active_sandbox(sandbox_id).await?;
        self.metrics.inc_destroy();
        info!(sandbox_id = %sandbox_id.0, "runtime sandbox destroyed");
        Ok(())
    }

    pub async fn active_count(&self) -> usize {
        self.sandboxes.lock().await.len()
    }

    pub async fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot().await
    }

    pub async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        note: Option<String>,
    ) -> Result<SnapshotMetadata> {
        self.ensure_hydrated().await?;
        info!(sandbox_id = %sandbox_id.0, note = ?note, "runtime create_snapshot");
        let sandbox = self
            .sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::SandboxNotFound(sandbox_id.0.to_string())
            })?;
        let snapshot = self
            .snapshots
            .create_snapshot(sandbox_id, &sandbox, note)
            .await?;
        let artifact_path = snapshot_artifact_path(&snapshot.id)?;
        self.backend
            .create_snapshot(sandbox_id, &snapshot.id, &artifact_path)
            .await?;
        if let Some(name) = sandbox.affinity_name.as_deref() {
            self.snapshots
                .set_affinity_snapshot(name, &snapshot.id)
                .await?;
        }
        info!(sandbox_id = %sandbox_id.0, snapshot_id = %snapshot.id.0, "runtime snapshot created");
        Ok(snapshot)
    }

    pub async fn restore_snapshot(&self, snapshot_id: &SnapshotId) -> Result<SandboxInfo> {
        self.ensure_hydrated().await?;
        warn!(snapshot_id = %snapshot_id.0, "runtime restore_snapshot requested");
        let snapshot = self
            .snapshots
            .get_snapshot(snapshot_id)
            .await?
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::ExecutionFailed("snapshot not found".to_string())
            })?;

        let artifact_path = snapshot_artifact_path(snapshot_id)?;
        if !artifact_path.exists() {
            return Err(HyperboxError::ExecutionFailed(format!(
                "snapshot artifact missing for {} at {}",
                snapshot_id.0,
                artifact_path.display()
            )));
        }

        let lease = self
            .backend
            .restore_snapshot(snapshot_id, &artifact_path, snapshot.config.clone())
            .await?;
        if let Some(name) = snapshot.config.affinity_name.as_deref() {
            self.snapshots.bind_sandbox(name, &lease.id).await?;
        }
        self.snapshots
            .upsert_active_sandbox(&lease.id, &snapshot.config, lease.info.created_at)
            .await?;
        self.sandboxes
            .lock()
            .await
            .insert(lease.id.clone(), snapshot.config.clone());
        self.metrics.inc_create();
        warn!(snapshot_id = %snapshot_id.0, sandbox_id = %lease.id.0, "runtime restore_snapshot restored sandbox from artifact");
        Ok(lease.info)
    }

    pub async fn list_snapshots(&self, template: &str) -> Result<Vec<SnapshotMetadata>> {
        self.ensure_hydrated().await?;
        self.snapshots.list_for_template(template).await
    }

    pub async fn resolve_affinity(
        &self,
        name: &str,
        restore_if_needed: bool,
    ) -> Result<(SandboxInfo, bool)> {
        self.ensure_hydrated().await?;
        let affinity =
            self.snapshots.get_affinity(name).await?.ok_or_else(|| {
                HyperboxError::ExecutionFailed(format!("affinity not found: {name}"))
            })?;

        if let Some(sandbox_id) = affinity.sandbox_id {
            match self.inspect(&sandbox_id).await {
                Ok(info) => return Ok((info, false)),
                Err(err) => {
                    warn!(name = %name, sandbox_id = %sandbox_id.0, error = %err, "affinity sandbox missing, clearing stale binding");
                    self.snapshots.clear_sandbox_binding(&sandbox_id).await?;
                }
            }
        }

        if !restore_if_needed {
            return Err(HyperboxError::ExecutionFailed(format!(
                "affinity `{name}` has no active sandbox"
            )));
        }

        let snapshot_id = affinity.snapshot_id.ok_or_else(|| {
            HyperboxError::ExecutionFailed(format!("affinity `{name}` has no snapshot to restore"))
        })?;
        let info = self.restore_snapshot(&snapshot_id).await?;
        Ok((info, true))
    }
}

fn snapshot_artifact_path(snapshot_id: &SnapshotId) -> Result<PathBuf> {
    let root = if let Ok(value) = std::env::var("HYPERBOX_SNAPSHOT_ROOT") {
        PathBuf::from(value)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".hyperbox/snapshots")
    } else {
        std::env::temp_dir().join("hyperbox/snapshots")
    };
    std::fs::create_dir_all(&root)?;
    Ok(root.join(format!("{}.tar.gz", snapshot_id.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalBackend;
    use std::sync::Arc;

    #[tokio::test]
    async fn server_lifecycle_works() {
        let backend = Arc::new(LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-server-lifecycle-test"),
        )));
        let server = HyperboxServer::new(backend);

        let info = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let out = server
            .exec(
                &info.id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        "echo ok".to_string(),
                    ],
                    timeout_secs: 2,
                },
            )
            .await
            .expect("exec");

        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("ok"));
        assert_eq!(server.active_count().await, 1);

        server
            .destroy_sandbox(&info.id)
            .await
            .expect("destroy sandbox");
        assert_eq!(server.active_count().await, 0);
    }

    #[tokio::test]
    async fn list_sandboxes_returns_active_sandbox_ids() {
        let backend = Arc::new(LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-server-list-test"),
        )));
        let server = HyperboxServer::new(backend);

        let info = server
            .create_sandbox(SandboxConfig {
                affinity_name: Some("list-test".to_string()),
                ..SandboxConfig::default()
            })
            .await
            .expect("create sandbox");

        let rows = server.list_sandboxes().await;
        assert!(rows.iter().any(|row| row.info.id == info.id));
        assert!(
            rows.iter()
                .any(|row| row.affinity_name.as_deref() == Some("list-test"))
        );
    }

    #[tokio::test]
    async fn restore_snapshot_fails_when_artifact_is_missing() {
        let backend = Arc::new(LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-server-restore-missing-artifact"),
        )));
        let snapshots = Arc::new(crate::InMemorySnapshotStore::default());
        let server = HyperboxServer::new_with_snapshots(backend, snapshots.clone());

        let info = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");
        let snapshot = snapshots
            .create_snapshot(
                &info.id,
                &SandboxConfig::default(),
                Some("no-artifact".to_string()),
            )
            .await
            .expect("create metadata only snapshot");

        let err = server
            .restore_snapshot(&snapshot.id)
            .await
            .expect_err("restore should fail when artifact is missing");
        assert!(err.to_string().contains("snapshot artifact missing"));
    }
}

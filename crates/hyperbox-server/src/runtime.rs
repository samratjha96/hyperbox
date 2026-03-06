use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;
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
        }
    }

    pub fn templates(&self) -> Vec<String> {
        self.templates
            .list()
            .into_iter()
            .map(|template| template.name.clone())
            .collect()
    }

    pub async fn create_sandbox(&self, config: SandboxConfig) -> Result<SandboxInfo> {
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
        self.sandboxes.lock().await.insert(lease.id.clone(), config);
        self.metrics.inc_create();
        info!(sandbox_id = %lease.id.0, template = %lease.info.template, "runtime sandbox created");
        Ok(lease.info)
    }

    pub async fn exec(&self, sandbox_id: &SandboxId, request: ExecRequest) -> Result<ExecOutcome> {
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
        self.backend.inspect(sandbox_id).await
    }

    pub async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        self.backend.read_file(sandbox_id, path).await
    }

    pub async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()> {
        self.backend.write_file(sandbox_id, payload).await
    }

    pub async fn destroy_sandbox(&self, sandbox_id: &SandboxId) -> Result<()> {
        info!(sandbox_id = %sandbox_id.0, "runtime destroy_sandbox");
        self.backend.destroy(sandbox_id).await?;
        self.sandboxes.lock().await.remove(sandbox_id);
        self.snapshots.clear_sandbox_binding(sandbox_id).await?;
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
        if let Some(name) = sandbox.affinity_name.as_deref() {
            self.snapshots
                .set_affinity_snapshot(name, &snapshot.id)
                .await?;
        }
        info!(sandbox_id = %sandbox_id.0, snapshot_id = %snapshot.id.0, "runtime snapshot created");
        Ok(snapshot)
    }

    pub async fn restore_snapshot(&self, snapshot_id: &SnapshotId) -> Result<SandboxInfo> {
        warn!(snapshot_id = %snapshot_id.0, "runtime restore_snapshot requested");
        let snapshot = self
            .snapshots
            .get_snapshot(snapshot_id)
            .await?
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::ExecutionFailed("snapshot not found".to_string())
            })?;

        self.create_sandbox(snapshot.config.clone())
        .await
        .map(|info| {
            warn!(snapshot_id = %snapshot_id.0, sandbox_id = %info.id.0, "runtime restore_snapshot created replacement sandbox");
            info
        })
    }

    pub async fn list_snapshots(&self, template: &str) -> Result<Vec<SnapshotMetadata>> {
        self.snapshots.list_for_template(template).await
    }

    pub async fn resolve_affinity(
        &self,
        name: &str,
        restore_if_needed: bool,
    ) -> Result<(SandboxInfo, bool)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalBackend;

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
}

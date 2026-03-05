use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

use hyperbox_core::{
    ExecOutcome, ExecRequest, FilePayload, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SnapshotId, SnapshotMetadata, SnapshotStore, TemplateRegistry,
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
        Self::new_with_snapshots(backend, Arc::new(crate::InMemorySnapshotStore::default()))
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
        let lease = self.backend.create(config.clone()).await?;
        self.sandboxes.lock().await.insert(lease.id.clone(), config);
        self.metrics.inc_create();
        Ok(lease.info)
    }

    pub async fn exec(&self, sandbox_id: &SandboxId, request: ExecRequest) -> Result<ExecOutcome> {
        self.metrics.inc_exec();
        let outcome = self.backend.exec(sandbox_id, request).await;
        match &outcome {
            Ok(outcome) => self.metrics.record_exec_latency(outcome.duration_ms).await,
            Err(_) => self.metrics.inc_exec_failure(),
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
        self.backend.destroy(sandbox_id).await?;
        self.sandboxes.lock().await.remove(sandbox_id);
        self.metrics.inc_destroy();
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
        let sandbox = self
            .sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::SandboxNotFound(sandbox_id.0.to_string())
            })?;
        self.snapshots
            .create_snapshot(sandbox_id, &sandbox.template, note)
            .await
    }

    pub async fn restore_snapshot(&self, snapshot_id: &SnapshotId) -> Result<SandboxInfo> {
        let snapshot = self
            .snapshots
            .get_snapshot(snapshot_id)
            .await?
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::ExecutionFailed("snapshot not found".to_string())
            })?;

        self.create_sandbox(SandboxConfig {
            template: snapshot.template,
            ..SandboxConfig::default()
        })
        .await
    }

    pub async fn list_snapshots(&self, template: &str) -> Result<Vec<SnapshotMetadata>> {
        self.snapshots.list_for_template(template).await
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

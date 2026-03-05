use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

use hyperbox_core::{
    ExecOutcome, ExecRequest, Result, SandboxBackend, SandboxConfig, SandboxId, SandboxInfo,
    TemplateRegistry,
};

#[derive(Clone)]
pub struct HyperboxServer {
    backend: Arc<dyn SandboxBackend>,
    templates: TemplateRegistry,
    sandboxes: Arc<Mutex<HashMap<SandboxId, SandboxConfig>>>,
}

impl HyperboxServer {
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        Self {
            backend,
            templates: TemplateRegistry::with_defaults(),
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
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
        Ok(lease.info)
    }

    pub async fn exec(&self, sandbox_id: &SandboxId, request: ExecRequest) -> Result<ExecOutcome> {
        self.backend.exec(sandbox_id, request).await
    }

    pub async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.backend.inspect(sandbox_id).await
    }

    pub async fn destroy_sandbox(&self, sandbox_id: &SandboxId) -> Result<()> {
        self.backend.destroy(sandbox_id).await?;
        self.sandboxes.lock().await.remove(sandbox_id);
        Ok(())
    }

    pub async fn active_count(&self) -> usize {
        self.sandboxes.lock().await.len()
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
                    command: vec!["/bin/sh".to_string(), "-lc".to_string(), "echo ok".to_string()],
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

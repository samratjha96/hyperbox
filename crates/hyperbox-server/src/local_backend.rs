use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use chrono::Utc;
use hyperbox_core::{
    FilePayload, HyperboxError, NetworkMode, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SandboxLease, SandboxState,
};
use tokio::{
    fs,
    process::Command,
    sync::Mutex,
    time::{Duration, timeout},
};
use tracing::{debug, info};

#[derive(Debug, Clone)]
struct SandboxRecord {
    config: SandboxConfig,
    info: SandboxInfo,
    working_dir: PathBuf,
    managed_working_dir: bool,
}

#[derive(Debug, Clone)]
pub struct LocalBackend {
    state: Arc<Mutex<HashMap<SandboxId, SandboxRecord>>>,
    root_dir: PathBuf,
}

impl LocalBackend {
    pub fn new(root_dir: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            root_dir: root_dir.unwrap_or_else(|| std::env::temp_dir().join("hyperbox-local")),
        }
    }

    fn sandbox_workdir(&self, sandbox_id: &SandboxId) -> PathBuf {
        self.root_dir.join(sandbox_id.0.to_string())
    }
}

#[async_trait::async_trait]
impl SandboxBackend for LocalBackend {
    async fn create(&self, config: SandboxConfig) -> Result<SandboxLease> {
        if !matches!(config.network, NetworkMode::None) {
            let allow_unsafe = std::env::var("HYPERBOX_LOCAL_ALLOW_UNENFORCED_NETWORK")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !allow_unsafe {
                return Err(HyperboxError::InvalidConfig(
                    "local backend does not enforce network policy; set HYPERBOX_BACKEND=firecracker for allowlist/full network".to_string(),
                ));
            }
        }

        fs::create_dir_all(&self.root_dir).await?;

        let id = SandboxId::new();
        let info = SandboxInfo {
            id: id.clone(),
            template: config.template.clone(),
            state: SandboxState::Ready,
            created_at: Utc::now(),
        };

        let (working_dir, managed_working_dir) = if let Some(workspace_dir) = &config.workspace_dir
        {
            let candidate = PathBuf::from(workspace_dir);
            let resolved = if candidate.is_absolute() {
                candidate
            } else {
                std::env::current_dir()?.join(candidate)
            };
            fs::create_dir_all(&resolved).await?;
            (resolved, false)
        } else {
            let path = self.sandbox_workdir(&id);
            fs::create_dir_all(&path).await?;
            (path, true)
        };

        let record = SandboxRecord {
            config,
            info: info.clone(),
            working_dir: working_dir.clone(),
            managed_working_dir,
        };

        self.state.lock().await.insert(id.clone(), record);
        info!(
            sandbox_id = %id.0,
            template = %info.template,
            workdir = %working_dir.display(),
            managed_workdir = managed_working_dir,
            "local backend sandbox created"
        );

        Ok(SandboxLease { id, info })
    }

    async fn exec(
        &self,
        sandbox_id: &SandboxId,
        req: hyperbox_core::ExecRequest,
    ) -> Result<hyperbox_core::ExecOutcome> {
        let record = self
            .state
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if req.command.is_empty() {
            return Err(HyperboxError::InvalidConfig(
                "command cannot be empty".to_string(),
            ));
        }
        debug!(
            sandbox_id = %sandbox_id.0,
            command = %req.command.join(" "),
            timeout_secs = req.timeout_secs,
            "local backend exec"
        );

        let program = &req.command[0];
        let args = &req.command[1..];

        let mut command = Command::new(program);
        command.args(args);
        command.current_dir(&record.working_dir);
        command.env("HYPERBOX_TEMPLATE", &record.config.template);
        command.env(
            "HYPERBOX_NETWORK_MODE",
            serde_json::to_string(&record.config.network)
                .unwrap_or_else(|_| "\"none\"".to_string()),
        );

        for (key, value) in &record.config.env {
            command.env(key, value);
        }

        let start = Instant::now();
        let output = timeout(Duration::from_secs(req.timeout_secs), command.output())
            .await
            .map_err(|_| HyperboxError::ExecutionFailed("command timed out".to_string()))??;

        let duration_ms = start.elapsed().as_millis();
        info!(
            sandbox_id = %sandbox_id.0,
            exit_code = output.status.code().unwrap_or(1),
            duration_ms,
            "local backend exec completed"
        );

        Ok(hyperbox_core::ExecOutcome {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms,
        })
    }

    async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        let record = self
            .state
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        let full_path = record.working_dir.join(path);
        let bytes = fs::read(&full_path).await?;

        Ok(FilePayload {
            path: camino::Utf8PathBuf::from_path_buf(PathBuf::from(path)).map_err(|_| {
                HyperboxError::InvalidConfig(format!("path is not valid utf8: {path}"))
            })?,
            bytes,
        })
    }

    async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()> {
        let record = self
            .state
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        let full_path = record.working_dir.join(payload.path.as_str());
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(full_path, payload.bytes).await?;
        Ok(())
    }

    async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()> {
        let record = self
            .state
            .lock()
            .await
            .remove(sandbox_id)
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))?;

        if record.managed_working_dir && record.working_dir.exists() {
            fs::remove_dir_all(record.working_dir).await?;
        }
        info!(sandbox_id = %sandbox_id.0, "local backend sandbox destroyed");

        Ok(())
    }

    async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.state
            .lock()
            .await
            .get(sandbox_id)
            .map(|record| record.info.clone())
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use hyperbox_core::{ExecRequest, NetworkMode, SandboxConfig, SandboxState};

    use super::*;

    #[tokio::test]
    async fn local_backend_executes_commands() {
        let backend = LocalBackend::new(Some(std::env::temp_dir().join("hyperbox-local-test")));
        let lease = backend
            .create(SandboxConfig {
                template: "python:3.12".to_string(),
                network: NetworkMode::None,
                ..SandboxConfig::default()
            })
            .await
            .expect("create sandbox");

        let out = backend
            .exec(
                &lease.id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        "echo hello".to_string(),
                    ],
                    timeout_secs: 2,
                },
            )
            .await
            .expect("exec command");

        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("hello"));
        assert!(matches!(lease.info.state, SandboxState::Ready));

        backend.destroy(&lease.id).await.expect("destroy sandbox");
    }

    #[tokio::test]
    async fn local_backend_honors_workspace_dir_without_cleanup() {
        let workspace =
            std::env::temp_dir().join(format!("hyperbox-workspace-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&workspace)
            .await
            .expect("create workspace");
        fs::write(workspace.join("marker.txt"), b"ok")
            .await
            .expect("write marker");

        let backend = LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-local-workspace-test"),
        ));
        let lease = backend
            .create(SandboxConfig {
                workspace_dir: Some(workspace.to_string_lossy().to_string()),
                ..SandboxConfig::default()
            })
            .await
            .expect("create sandbox");

        let out = backend
            .exec(
                &lease.id,
                ExecRequest {
                    command: vec!["/bin/sh".to_string(), "-lc".to_string(), "pwd".to_string()],
                    timeout_secs: 2,
                },
            )
            .await
            .expect("exec");

        let expected = std::fs::canonicalize(&workspace).expect("canonical workspace");
        let actual = std::fs::canonicalize(out.stdout.trim_end()).expect("canonical exec pwd");
        assert_eq!(actual, expected);
        assert!(workspace.join("marker.txt").exists());

        backend.destroy(&lease.id).await.expect("destroy sandbox");
        assert!(workspace.exists());
        assert!(workspace.join("marker.txt").exists());

        fs::remove_dir_all(&workspace)
            .await
            .expect("cleanup workspace");
    }

    #[tokio::test]
    async fn local_backend_rejects_networked_modes_by_default() {
        let backend = LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-local-network-reject-test"),
        ));

        let result = backend
            .create(SandboxConfig {
                network: NetworkMode::Allowlist(vec!["pypi.org".to_string()]),
                ..SandboxConfig::default()
            })
            .await;

        assert!(result.is_err());
        let err = result.expect_err("expected networked mode rejection");
        assert!(
            err.to_string().contains("does not enforce network policy"),
            "unexpected error: {err}"
        );
    }
}

use async_trait::async_trait;
use camino::Utf8PathBuf;

use crate::{
    config::SandboxConfig,
    model::{ExecOutcome, ExecRequest, SandboxId, SandboxInfo},
    Result,
};

#[derive(Debug, Clone)]
pub struct FilePayload {
    pub path: Utf8PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SandboxLease {
    pub id: SandboxId,
    pub info: SandboxInfo,
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    async fn create(&self, config: SandboxConfig) -> Result<SandboxLease>;
    async fn exec(&self, sandbox_id: &SandboxId, req: ExecRequest) -> Result<ExecOutcome>;
    async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload>;
    async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()>;
    async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()>;
    async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo>;
}

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Result, SandboxConfig, SandboxId};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub Uuid);

impl SnapshotId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SnapshotId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMetadata {
    pub id: SnapshotId,
    pub sandbox_id: SandboxId,
    pub affinity_name: Option<String>,
    pub template: String,
    pub config: SandboxConfig,
    pub created_at: DateTime<Utc>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AffinityRecord {
    pub name: String,
    pub sandbox_id: Option<SandboxId>,
    pub snapshot_id: Option<SnapshotId>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveSandboxRecord {
    pub sandbox_id: SandboxId,
    pub config: SandboxConfig,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
        note: Option<String>,
    ) -> Result<SnapshotMetadata>;

    async fn get_snapshot(&self, snapshot_id: &SnapshotId) -> Result<Option<SnapshotMetadata>>;

    async fn list_for_template(&self, template: &str) -> Result<Vec<SnapshotMetadata>>;

    async fn bind_sandbox(&self, name: &str, sandbox_id: &SandboxId) -> Result<()>;

    async fn clear_sandbox_binding(&self, sandbox_id: &SandboxId) -> Result<()>;

    async fn set_affinity_snapshot(&self, name: &str, snapshot_id: &SnapshotId) -> Result<()>;

    async fn get_affinity(&self, name: &str) -> Result<Option<AffinityRecord>>;

    async fn upsert_active_sandbox(
        &self,
        sandbox_id: &SandboxId,
        config: &SandboxConfig,
        created_at: DateTime<Utc>,
    ) -> Result<()>;

    async fn remove_active_sandbox(&self, sandbox_id: &SandboxId) -> Result<()>;

    async fn list_active_sandboxes(&self) -> Result<Vec<ActiveSandboxRecord>>;
}

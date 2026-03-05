use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Result, SandboxId};

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
    pub template: String,
    pub created_at: DateTime<Utc>,
    pub note: Option<String>,
}

#[async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        template: &str,
        note: Option<String>,
    ) -> Result<SnapshotMetadata>;

    async fn get_snapshot(&self, snapshot_id: &SnapshotId) -> Result<Option<SnapshotMetadata>>;

    async fn list_for_template(&self, template: &str) -> Result<Vec<SnapshotMetadata>>;
}

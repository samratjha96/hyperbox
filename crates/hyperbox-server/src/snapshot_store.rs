use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use tokio::sync::Mutex;

use hyperbox_core::{Result, SandboxId, SnapshotId, SnapshotMetadata, SnapshotStore};

#[derive(Debug, Clone, Default)]
pub struct InMemorySnapshotStore {
    snapshots: Arc<Mutex<HashMap<SnapshotId, SnapshotMetadata>>>,
}

#[async_trait::async_trait]
impl SnapshotStore for InMemorySnapshotStore {
    async fn create_snapshot(
        &self,
        sandbox_id: &SandboxId,
        template: &str,
        note: Option<String>,
    ) -> Result<SnapshotMetadata> {
        let snapshot = SnapshotMetadata {
            id: SnapshotId::new(),
            sandbox_id: sandbox_id.clone(),
            template: template.to_string(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_store_roundtrip() {
        let store = InMemorySnapshotStore::default();
        let sandbox_id = SandboxId::new();

        let created = store
            .create_snapshot(&sandbox_id, "python:3.12", Some("warm start".to_string()))
            .await
            .expect("create snapshot");

        let found = store
            .get_snapshot(&created.id)
            .await
            .expect("get snapshot")
            .expect("snapshot exists");

        assert_eq!(found.template, "python:3.12");
    }
}

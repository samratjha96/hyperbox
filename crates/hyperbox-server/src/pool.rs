use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
};

use tokio::sync::Mutex;

use hyperbox_core::{Result, SandboxBackend, SandboxConfig, SandboxId};

#[derive(Debug, Clone)]
pub struct PoolStats {
    pub available: usize,
    pub in_use: usize,
    pub target: usize,
}

#[derive(Clone)]
pub struct WarmPoolManager<B: SandboxBackend> {
    backend: Arc<B>,
    config: SandboxConfig,
    target_size: usize,
    available: Arc<Mutex<VecDeque<SandboxId>>>,
    in_use: Arc<Mutex<HashSet<SandboxId>>>,
}

impl<B: SandboxBackend> WarmPoolManager<B> {
    pub fn new(backend: Arc<B>, config: SandboxConfig, target_size: usize) -> Self {
        Self {
            backend,
            config,
            target_size,
            available: Arc::new(Mutex::new(VecDeque::new())),
            in_use: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn fill(&self) -> Result<()> {
        loop {
            let current = self.available.lock().await.len();
            if current >= self.target_size {
                break;
            }

            let lease = self.backend.create(self.config.clone()).await?;
            self.available.lock().await.push_back(lease.id);
        }
        Ok(())
    }

    pub async fn checkout(&self) -> Result<SandboxId> {
        if self.available.lock().await.is_empty() {
            self.fill().await?;
        }

        let id = self
            .available
            .lock()
            .await
            .pop_front()
            .expect("pool refill should create at least one sandbox");

        self.in_use.lock().await.insert(id.clone());
        Ok(id)
    }

    pub async fn release(&self, id: SandboxId) {
        let mut in_use = self.in_use.lock().await;
        if in_use.remove(&id) {
            drop(in_use);
            self.available.lock().await.push_back(id);
        }
    }

    pub async fn stats(&self) -> PoolStats {
        PoolStats {
            available: self.available.lock().await.len(),
            in_use: self.in_use.lock().await.len(),
            target: self.target_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalBackend;

    #[tokio::test]
    async fn pool_warms_and_reuses() {
        let backend = Arc::new(LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-pool-test"),
        )));
        let pool = WarmPoolManager::new(backend, SandboxConfig::default(), 2);

        pool.fill().await.expect("fill pool");
        let first = pool.checkout().await.expect("checkout first");
        pool.release(first).await;

        let stats = pool.stats().await;
        assert_eq!(stats.target, 2);
        assert_eq!(stats.in_use, 0);
        assert!(stats.available >= 1);
    }
}

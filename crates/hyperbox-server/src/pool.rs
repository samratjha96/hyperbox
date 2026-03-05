use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    sync::Arc,
    time::Duration,
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

impl<B: SandboxBackend + 'static> WarmPoolManager<B> {
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

    pub async fn checkout_or_restore<F, Fut>(&self, restore: F) -> Result<SandboxId>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<SandboxId>> + Send,
    {
        if let Some(id) = self.available.lock().await.pop_front() {
            self.in_use.lock().await.insert(id.clone());
            return Ok(id);
        }

        let restored = restore().await?;
        self.in_use.lock().await.insert(restored.clone());
        Ok(restored)
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

    pub fn start_auto_refill(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = self.fill().await;
                tokio::time::sleep(interval).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperbox_core::SandboxId;

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

    #[tokio::test]
    async fn checkout_or_restore_calls_restore_when_empty() {
        let backend = Arc::new(LocalBackend::new(Some(
            std::env::temp_dir().join("hyperbox-pool-restore-test"),
        )));
        let pool = WarmPoolManager::new(backend, SandboxConfig::default(), 0);

        let id = pool
            .checkout_or_restore(|| async { Ok(SandboxId::new()) })
            .await
            .expect("checkout or restore");

        assert_ne!(id.0, uuid::Uuid::nil());
    }
}

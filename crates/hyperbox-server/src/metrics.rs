use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct MetricsCollector {
    creates: Arc<AtomicU64>,
    destroys: Arc<AtomicU64>,
    execs: Arc<AtomicU64>,
    exec_failures: Arc<AtomicU64>,
    exec_latency_ms: Arc<Mutex<Vec<u128>>>,
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub creates: u64,
    pub destroys: u64,
    pub execs: u64,
    pub exec_failures: u64,
    pub p50_exec_ms: u128,
    pub p95_exec_ms: u128,
}

impl MetricsCollector {
    pub fn inc_create(&self) {
        self.creates.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_destroy(&self) {
        self.destroys.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_exec(&self) {
        self.execs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_exec_failure(&self) {
        self.exec_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn record_exec_latency(&self, ms: u128) {
        self.exec_latency_ms.lock().await.push(ms);
    }

    pub async fn snapshot(&self) -> MetricsSnapshot {
        let mut latency = self.exec_latency_ms.lock().await.clone();
        latency.sort_unstable();

        MetricsSnapshot {
            creates: self.creates.load(Ordering::Relaxed),
            destroys: self.destroys.load(Ordering::Relaxed),
            execs: self.execs.load(Ordering::Relaxed),
            exec_failures: self.exec_failures.load(Ordering::Relaxed),
            p50_exec_ms: percentile(&latency, 50),
            p95_exec_ms: percentile(&latency, 95),
        }
    }
}

fn percentile(values: &[u128], p: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }

    let rank = ((values.len() - 1) * p) / 100;
    values[rank]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn computes_percentiles() {
        let metrics = MetricsCollector::default();
        metrics.record_exec_latency(5).await;
        metrics.record_exec_latency(50).await;
        metrics.record_exec_latency(100).await;

        let snap = metrics.snapshot().await;
        assert_eq!(snap.p50_exec_ms, 50);
        assert_eq!(snap.p95_exec_ms, 100);
    }
}

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::{Duration as ChronoDuration, Utc};
use tokio::sync::{Mutex, OnceCell};
use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

use hyperbox_core::{
    ExecOutcome, ExecRequest, FilePayload, HyperboxError, ProcessDisposition, ProcessId,
    ProcessInfo, ProcessLogRead, ProcessStatus, Result, SandboxBackend, SandboxConfig, SandboxId,
    SandboxInfo, SnapshotId, SnapshotMetadata, SnapshotStore, StreamName, TemplateRegistry,
};

use crate::metrics::{MetricsCollector, MetricsSnapshot};

#[derive(Clone)]
pub struct HyperboxServer {
    backend: Arc<dyn SandboxBackend>,
    templates: TemplateRegistry,
    sandboxes: Arc<Mutex<HashMap<SandboxId, SandboxConfig>>>,
    metrics: MetricsCollector,
    snapshots: Arc<dyn SnapshotStore>,
    hydration_complete: Arc<OnceCell<()>>,
}

#[derive(Debug, Clone)]
pub struct ActiveSandboxInfo {
    pub info: SandboxInfo,
    pub affinity_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedRunSandbox {
    pub info: SandboxInfo,
    pub requested_sandbox_id: Option<SandboxId>,
    pub disposition: ProcessDisposition,
}

#[derive(Debug, Clone)]
pub struct StartRunRequest {
    pub sandbox_id: Option<SandboxId>,
    pub affinity_name: Option<String>,
    pub create_config: Option<SandboxConfig>,
    pub reuse_auto_session: bool,
    pub ensure_commands: Vec<String>,
    pub writes: Vec<(String, Vec<u8>)>,
    pub command: String,
    pub destroy_sandbox_on_expiry: bool,
}

#[derive(Debug, Clone)]
pub struct StartedRun {
    pub process: ProcessInfo,
    pub sandbox: SandboxInfo,
    pub session_name: Option<String>,
    pub session_created: bool,
}

impl HyperboxServer {
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        if cfg!(test) {
            return Self::new_with_snapshots(
                backend,
                Arc::new(crate::InMemorySnapshotStore::default()),
            );
        }
        let snapshots: Arc<dyn SnapshotStore> = match crate::SqliteSnapshotStore::open_default() {
            Ok(store) => Arc::new(store),
            Err(err) => {
                warn!(error = %err, "failed to open sqlite snapshot store, falling back to in-memory");
                Arc::new(crate::InMemorySnapshotStore::default())
            }
        };
        Self::new_with_snapshots(backend, snapshots)
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
            hydration_complete: Arc::new(OnceCell::new()),
        }
    }

    pub fn templates(&self) -> Vec<String> {
        self.templates
            .list()
            .into_iter()
            .map(|template| template.name.clone())
            .collect()
    }

    async fn ensure_hydrated(&self) -> Result<()> {
        self.hydration_complete
            .get_or_try_init(|| async {
                let records = self.snapshots.list_active_sandboxes().await?;
                let mut retry_needed = false;
                for record in records {
                    let sandbox_id = record.sandbox_id.clone();
                    match self.backend.inspect(&sandbox_id).await {
                        Ok(_) => {
                            self.sandboxes
                                .lock()
                                .await
                                .insert(sandbox_id, record.config.clone());
                        }
                        Err(err) => {
                            if matches!(err, HyperboxError::SandboxNotFound(_)) {
                                warn!(
                                    sandbox_id = %record.sandbox_id.0,
                                    error = %err,
                                    "hydration could not verify sandbox; keeping persisted record"
                                );
                            } else {
                                retry_needed = true;
                                warn!(
                                    sandbox_id = %record.sandbox_id.0,
                                    error = %err,
                                    "hydration encountered transient failure; will retry"
                                );
                            }
                        }
                    }
                }
                if retry_needed {
                    return Err(HyperboxError::ExecutionFailed(
                        "hydration incomplete; retrying on next request".to_string(),
                    ));
                }
                Ok(())
            })
            .await
            .map(|_| ())
    }

    pub async fn create_sandbox(&self, config: SandboxConfig) -> Result<SandboxInfo> {
        self.ensure_hydrated().await?;
        self.templates.ensure_exists(&config.template)?;
        info!(
            template = %config.template,
            memory_mb = config.memory_mb,
            vcpu_count = config.vcpu_count,
            "runtime create_sandbox"
        );
        self.backend.validate_config(&config)?;
        let lease = self.backend.create(config.clone()).await?;
        if let Some(name) = config.affinity_name.as_deref() {
            self.snapshots.bind_sandbox(name, &lease.id).await?;
        }
        self.snapshots
            .upsert_active_sandbox(&lease.id, &config, lease.info.created_at)
            .await?;
        self.sandboxes.lock().await.insert(lease.id.clone(), config);
        self.metrics.inc_create();
        info!(sandbox_id = %lease.id.0, template = %lease.info.template, "runtime sandbox created");
        Ok(lease.info)
    }

    pub async fn start_process(
        &self,
        sandbox_id: &SandboxId,
        command: Vec<String>,
        requested_sandbox_id: Option<SandboxId>,
        disposition: ProcessDisposition,
        destroy_sandbox_on_expiry: bool,
    ) -> Result<ProcessInfo> {
        self.ensure_hydrated().await?;
        self.cleanup_expired_processes().await?;
        if command.is_empty() {
            return Err(HyperboxError::InvalidConfig(
                "managed process command cannot be empty".to_string(),
            ));
        }

        if let Some(existing) = self.active_process_for_sandbox(sandbox_id).await? {
            return Err(HyperboxError::ExecutionFailed(format!(
                "sandbox {} already has a running managed process ({})",
                sandbox_id.0, existing.id.0
            )));
        }

        let process_id = ProcessId::new();
        let process = ProcessInfo {
            id: process_id.clone(),
            sandbox_id: sandbox_id.clone(),
            requested_sandbox_id,
            disposition,
            destroy_sandbox_on_expiry,
            command: command.clone(),
            status: ProcessStatus::Starting,
            stdout_path: process_stdout_path(&process_id),
            stderr_path: process_stderr_path(&process_id),
            backend_pid: None,
            exit_code: None,
            started_at: Utc::now(),
            finished_at: None,
            expires_at: None,
        };
        self.snapshots.upsert_process(&process).await?;

        let launch = self
            .backend
            .exec(
                sandbox_id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        process_launch_command(&process)?,
                    ],
                    timeout_secs: 5,
                },
            )
            .await?;
        if launch.exit_code != 0 {
            let mut failed = process.clone();
            failed.status = ProcessStatus::Failed;
            failed.exit_code = Some(launch.exit_code);
            failed.finished_at = Some(Utc::now());
            failed.expires_at = Some(process_expiry_at(Utc::now()));
            self.snapshots.upsert_process(&failed).await?;
            return Err(HyperboxError::ExecutionFailed(format!(
                "failed to launch managed process: {}",
                launch.stderr.trim()
            )));
        }

        let mut running = process;
        running.status = ProcessStatus::Running;
        self.snapshots.upsert_process(&running).await?;
        Ok(running)
    }

    pub async fn start_run(&self, request: StartRunRequest) -> Result<StartedRun> {
        self.ensure_hydrated().await?;
        self.cleanup_expired_processes().await?;
        if request.command.trim().is_empty() {
            return Err(HyperboxError::InvalidConfig(
                "run command cannot be empty".to_string(),
            ));
        }

        let mut session_name = None;
        let mut session_created = false;
        let prepared = if let Some(sandbox_id) = request.sandbox_id.as_ref() {
            let config = self.sandbox_config(sandbox_id).await?;
            self.prepare_run_sandbox(sandbox_id, Some(config)).await?
        } else if let Some(name) = request.affinity_name.as_deref() {
            let (sandbox, _) = self.resolve_affinity(name, true).await?;
            let config = self.sandbox_config(&sandbox.id).await?;
            self.prepare_run_sandbox(&sandbox.id, Some(config)).await?
        } else if let Some(mut config) = request.create_config.clone() {
            if request.reuse_auto_session {
                let derived = derive_auto_run_affinity_name(
                    &config.template,
                    config.workspace_dir.as_deref(),
                    &config.network,
                );
                session_name = Some(derived.clone());
                config.affinity_name = Some(derived);
                match self
                    .resolve_affinity(session_name.as_deref().unwrap_or_default(), false)
                    .await
                {
                    Ok((sandbox, _)) => {
                        let mut overflow_config = self.sandbox_config(&sandbox.id).await?;
                        overflow_config.affinity_name = None;
                        self.prepare_run_sandbox(&sandbox.id, Some(overflow_config))
                            .await?
                    }
                    Err(err) if is_affinity_absent_error(&err) => {
                        let sandbox = self.create_sandbox(config).await?;
                        session_created = true;
                        PreparedRunSandbox {
                            info: sandbox,
                            requested_sandbox_id: None,
                            disposition: ProcessDisposition::CreatedNew,
                        }
                    }
                    Err(err) => return Err(err),
                }
            } else {
                let sandbox = self.create_sandbox(config).await?;
                PreparedRunSandbox {
                    info: sandbox,
                    requested_sandbox_id: None,
                    disposition: ProcessDisposition::CreatedNew,
                }
            }
        } else {
            return Err(HyperboxError::InvalidConfig(
                "run target requires sandbox_id, affinity_name, or create_config".to_string(),
            ));
        };

        for (path, bytes) in &request.writes {
            self.backend
                .write_file(
                    &prepared.info.id,
                    FilePayload {
                        path: path.clone().into(),
                        bytes: bytes.clone(),
                    },
                )
                .await?;
        }

        let timeout_secs = self.sandbox_config(&prepared.info.id).await?.timeout_secs;
        self.run_ensure_commands(&prepared.info.id, &request.ensure_commands, timeout_secs)
            .await?;

        let process = self
            .start_process(
                &prepared.info.id,
                vec!["/bin/sh".to_string(), "-lc".to_string(), request.command],
                prepared.requested_sandbox_id.clone(),
                prepared.disposition.clone(),
                request.destroy_sandbox_on_expiry
                    || prepared.disposition == ProcessDisposition::CreatedDueToBusy,
            )
            .await?;

        Ok(StartedRun {
            process,
            sandbox: prepared.info,
            session_name,
            session_created,
        })
    }

    pub async fn get_process(&self, process_id: &ProcessId) -> Result<ProcessInfo> {
        self.ensure_hydrated().await?;
        self.cleanup_expired_processes().await?;
        let process = self
            .snapshots
            .get_process(process_id)
            .await?
            .ok_or_else(|| {
                HyperboxError::ExecutionFailed(format!("process not found: {}", process_id.0))
            })?;
        self.refresh_process(process).await
    }

    pub async fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        self.ensure_hydrated().await?;
        self.cleanup_expired_processes().await?;
        let processes = self.snapshots.list_processes().await?;
        let mut refreshed = Vec::with_capacity(processes.len());
        for process in processes {
            if let Ok(process) = self.refresh_process(process).await {
                refreshed.push(process);
            }
        }
        refreshed.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(refreshed)
    }

    pub async fn wait_process(
        &self,
        process_id: &ProcessId,
        timeout_secs: u64,
    ) -> Result<ProcessInfo> {
        self.ensure_hydrated().await?;
        let started = std::time::Instant::now();
        let deadline = started + Duration::from_secs(timeout_secs.max(1));
        loop {
            let process = self.get_process(process_id).await?;
            if process.status.is_terminal() {
                return Ok(process);
            }
            if std::time::Instant::now() >= deadline {
                return Err(HyperboxError::ExecutionFailed(format!(
                    "timed out waiting for process {}",
                    process_id.0
                )));
            }
            sleep(process_wait_poll_interval(started.elapsed())).await;
        }
    }

    pub async fn read_process_log(
        &self,
        process_id: &ProcessId,
        stream: StreamName,
        offset: u64,
        limit: u64,
    ) -> Result<ProcessLogRead> {
        self.ensure_hydrated().await?;
        let process = self
            .snapshots
            .get_process(process_id)
            .await?
            .ok_or_else(|| {
                HyperboxError::ExecutionFailed(format!("process not found: {}", process_id.0))
            })?;
        let path = match stream {
            StreamName::Stdout => process.stdout_path.clone(),
            StreamName::Stderr => process.stderr_path.clone(),
        };
        let total = self.read_log_size(&process.sandbox_id, &path).await?;
        if offset >= total {
            let refreshed = self.refresh_process(process.clone()).await?;
            if !refreshed.status.is_terminal() {
                sleep(Duration::from_millis(50)).await;
                let retried_total = self.read_log_size(&process.sandbox_id, &path).await?;
                if retried_total > offset {
                    let chunk_limit = limit.max(1);
                    let outcome = self
                        .backend
                        .exec(
                            &process.sandbox_id,
                            ExecRequest {
                                command: vec![
                                    "/bin/sh".to_string(),
                                    "-lc".to_string(),
                                    format!(
                                        "path={}; if [ ! -f \"$path\" ]; then exit 0; fi; tail -c +{} \"$path\" | head -c {}",
                                        shell_escape(&path),
                                        offset + 1,
                                        chunk_limit
                                    ),
                                ],
                                timeout_secs: 5,
                            },
                        )
                        .await?;
                    let contents = outcome.stdout;
                    let next_offset = offset + contents.len() as u64;
                    return Ok(ProcessLogRead {
                        stream,
                        offset,
                        next_offset,
                        eof: next_offset >= retried_total,
                        contents,
                    });
                }
            }
            return Ok(ProcessLogRead {
                stream,
                offset,
                next_offset: offset,
                eof: refreshed.status.is_terminal(),
                contents: String::new(),
            });
        }

        let chunk_limit = limit.max(1);
        let outcome = self
            .backend
            .exec(
                &process.sandbox_id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        format!(
                            "path={}; if [ ! -f \"$path\" ]; then exit 0; fi; tail -c +{} \"$path\" | head -c {}",
                            shell_escape(&path),
                            offset + 1,
                            chunk_limit
                        ),
                    ],
                    timeout_secs: 5,
                },
            )
            .await?;
        let contents = outcome.stdout;
        let next_offset = offset + contents.len() as u64;
        Ok(ProcessLogRead {
            stream,
            offset,
            next_offset,
            eof: next_offset >= total,
            contents,
        })
    }

    pub async fn cancel_process(&self, process_id: &ProcessId) -> Result<ProcessInfo> {
        self.ensure_hydrated().await?;
        let process = self.get_process(process_id).await?;
        if process.status.is_terminal() {
            return Ok(process);
        }

        self.backend
            .write_file(
                &process.sandbox_id,
                FilePayload {
                    path: process_cancel_path(&process.id).into(),
                    bytes: Vec::new(),
                },
            )
            .await?;
        self.backend
            .exec(
                &process.sandbox_id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        cancel_script(&process),
                    ],
                    timeout_secs: 5,
                },
            )
            .await?;

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let refreshed = self.get_process(process_id).await?;
            if refreshed.status.is_terminal() {
                return Ok(refreshed);
            }
            if std::time::Instant::now() >= deadline {
                return Err(HyperboxError::ExecutionFailed(format!(
                    "timed out cancelling process {}",
                    process_id.0
                )));
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn exec(&self, sandbox_id: &SandboxId, request: ExecRequest) -> Result<ExecOutcome> {
        self.ensure_hydrated().await?;
        debug!(
            sandbox_id = %sandbox_id.0,
            timeout_secs = request.timeout_secs,
            command = %request.command.join(" "),
            "runtime exec"
        );
        self.metrics.inc_exec();
        let outcome = self.backend.exec(sandbox_id, request).await;
        match &outcome {
            Ok(outcome) => {
                self.metrics.record_exec_latency(outcome.duration_ms).await;
                info!(
                    sandbox_id = %sandbox_id.0,
                    exit_code = outcome.exit_code,
                    duration_ms = outcome.duration_ms,
                    "runtime exec completed"
                );
            }
            Err(err) => {
                self.metrics.inc_exec_failure();
                warn!(sandbox_id = %sandbox_id.0, error = %err, "runtime exec failed");
            }
        }
        outcome
    }

    pub async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
        self.ensure_hydrated().await?;
        self.backend.inspect(sandbox_id).await
    }

    pub async fn sandbox_config(&self, sandbox_id: &SandboxId) -> Result<SandboxConfig> {
        self.ensure_hydrated().await?;
        self.sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| HyperboxError::SandboxNotFound(sandbox_id.0.to_string()))
    }

    pub async fn list_sandboxes(&self) -> Vec<ActiveSandboxInfo> {
        if let Err(err) = self.ensure_hydrated().await {
            warn!(error = %err, "runtime list_sandboxes proceeding without hydration");
        }
        let entries: Vec<(SandboxId, Option<String>)> = self
            .sandboxes
            .lock()
            .await
            .iter()
            .map(|(id, config)| (id.clone(), config.affinity_name.clone()))
            .collect();

        let mut rows = Vec::with_capacity(entries.len());
        for (sandbox_id, affinity_name) in entries {
            match self.backend.inspect(&sandbox_id).await {
                Ok(info) => rows.push(ActiveSandboxInfo {
                    info,
                    affinity_name,
                }),
                Err(err) => {
                    warn!(
                        sandbox_id = %sandbox_id.0,
                        error = %err,
                        "runtime list_sandboxes skipping missing sandbox"
                    );
                }
            }
        }
        rows.sort_by(|a, b| a.info.created_at.cmp(&b.info.created_at));
        rows
    }

    pub async fn prepare_run_sandbox(
        &self,
        sandbox_id: &SandboxId,
        overflow_config: Option<SandboxConfig>,
    ) -> Result<PreparedRunSandbox> {
        self.ensure_hydrated().await?;
        self.cleanup_expired_processes().await?;

        if self.active_process_for_sandbox(sandbox_id).await?.is_none() {
            return Ok(PreparedRunSandbox {
                info: self.inspect(sandbox_id).await?,
                requested_sandbox_id: None,
                disposition: ProcessDisposition::ReusedExisting,
            });
        }

        let Some(mut overflow_config) = overflow_config else {
            let existing = self
                .active_process_for_sandbox(sandbox_id)
                .await?
                .ok_or_else(|| {
                    HyperboxError::ExecutionFailed(format!(
                        "sandbox {} became idle while preparing run target",
                        sandbox_id.0
                    ))
                })?;
            return Err(HyperboxError::ExecutionFailed(format!(
                "sandbox {} already has a running managed process ({})",
                sandbox_id.0, existing.id.0
            )));
        };
        overflow_config.affinity_name = None;
        let info = self.create_sandbox(overflow_config).await?;
        Ok(PreparedRunSandbox {
            info,
            requested_sandbox_id: Some(sandbox_id.clone()),
            disposition: ProcessDisposition::CreatedDueToBusy,
        })
    }

    pub async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
        self.ensure_hydrated().await?;
        self.backend.read_file(sandbox_id, path).await
    }

    pub async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()> {
        self.ensure_hydrated().await?;
        self.backend.write_file(sandbox_id, payload).await
    }

    pub async fn destroy_sandbox(&self, sandbox_id: &SandboxId) -> Result<()> {
        self.ensure_hydrated().await?;
        let processes = self.snapshots.list_processes().await?;
        for process in processes
            .into_iter()
            .filter(|process| process.sandbox_id == *sandbox_id)
        {
            let refreshed = self.refresh_process(process).await?;
            if !refreshed.status.is_terminal() {
                let _ = self.cancel_process(&refreshed.id).await;
            }
            self.purge_process(&refreshed).await?;
        }
        info!(sandbox_id = %sandbox_id.0, "runtime destroy_sandbox");
        self.backend.destroy(sandbox_id).await?;
        self.sandboxes.lock().await.remove(sandbox_id);
        self.snapshots.clear_sandbox_binding(sandbox_id).await?;
        self.snapshots.remove_active_sandbox(sandbox_id).await?;
        self.metrics.inc_destroy();
        info!(sandbox_id = %sandbox_id.0, "runtime sandbox destroyed");
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
        self.ensure_hydrated().await?;
        info!(sandbox_id = %sandbox_id.0, note = ?note, "runtime create_snapshot");
        let sandbox = self
            .sandboxes
            .lock()
            .await
            .get(sandbox_id)
            .cloned()
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::SandboxNotFound(sandbox_id.0.to_string())
            })?;
        let snapshot = self
            .snapshots
            .create_snapshot(sandbox_id, &sandbox, note)
            .await?;
        let artifact_path = snapshot_artifact_path(&snapshot.id)?;
        self.backend
            .create_snapshot(sandbox_id, &snapshot.id, &artifact_path)
            .await?;
        if let Some(name) = sandbox.affinity_name.as_deref() {
            self.snapshots
                .set_affinity_snapshot(name, &snapshot.id)
                .await?;
        }
        info!(sandbox_id = %sandbox_id.0, snapshot_id = %snapshot.id.0, "runtime snapshot created");
        Ok(snapshot)
    }

    pub async fn restore_snapshot(&self, snapshot_id: &SnapshotId) -> Result<SandboxInfo> {
        self.ensure_hydrated().await?;
        warn!(snapshot_id = %snapshot_id.0, "runtime restore_snapshot requested");
        let snapshot = self
            .snapshots
            .get_snapshot(snapshot_id)
            .await?
            .ok_or_else(|| {
                hyperbox_core::HyperboxError::ExecutionFailed("snapshot not found".to_string())
            })?;

        let artifact_path = snapshot_artifact_path(snapshot_id)?;
        if !artifact_path.exists() {
            return Err(HyperboxError::ExecutionFailed(format!(
                "snapshot artifact missing for {} at {}",
                snapshot_id.0,
                artifact_path.display()
            )));
        }

        let lease = self
            .backend
            .restore_snapshot(snapshot_id, &artifact_path, snapshot.config.clone())
            .await?;
        if let Some(name) = snapshot.config.affinity_name.as_deref() {
            self.snapshots.bind_sandbox(name, &lease.id).await?;
        }
        self.snapshots
            .upsert_active_sandbox(&lease.id, &snapshot.config, lease.info.created_at)
            .await?;
        self.sandboxes
            .lock()
            .await
            .insert(lease.id.clone(), snapshot.config.clone());
        self.metrics.inc_create();
        warn!(snapshot_id = %snapshot_id.0, sandbox_id = %lease.id.0, "runtime restore_snapshot restored sandbox from artifact");
        Ok(lease.info)
    }

    pub async fn list_snapshots(&self, template: &str) -> Result<Vec<SnapshotMetadata>> {
        self.ensure_hydrated().await?;
        self.snapshots.list_for_template(template).await
    }

    pub async fn resolve_affinity(
        &self,
        name: &str,
        restore_if_needed: bool,
    ) -> Result<(SandboxInfo, bool)> {
        self.ensure_hydrated().await?;
        let affinity =
            self.snapshots.get_affinity(name).await?.ok_or_else(|| {
                HyperboxError::ExecutionFailed(format!("affinity not found: {name}"))
            })?;

        if let Some(sandbox_id) = affinity.sandbox_id {
            match self.inspect(&sandbox_id).await {
                Ok(info) => return Ok((info, false)),
                Err(err) => {
                    warn!(name = %name, sandbox_id = %sandbox_id.0, error = %err, "affinity sandbox missing, clearing stale binding");
                    self.snapshots.clear_sandbox_binding(&sandbox_id).await?;
                }
            }
        }

        if !restore_if_needed {
            return Err(HyperboxError::ExecutionFailed(format!(
                "affinity `{name}` has no active sandbox"
            )));
        }

        let snapshot_id = affinity.snapshot_id.ok_or_else(|| {
            HyperboxError::ExecutionFailed(format!("affinity `{name}` has no snapshot to restore"))
        })?;
        let info = self.restore_snapshot(&snapshot_id).await?;
        Ok((info, true))
    }

    async fn active_process_for_sandbox(
        &self,
        sandbox_id: &SandboxId,
    ) -> Result<Option<ProcessInfo>> {
        for process in self.snapshots.list_processes().await? {
            if process.sandbox_id != *sandbox_id {
                continue;
            }
            let refreshed = self.refresh_process(process).await?;
            if !refreshed.status.is_terminal() {
                return Ok(Some(refreshed));
            }
        }
        Ok(None)
    }

    async fn refresh_process(&self, process: ProcessInfo) -> Result<ProcessInfo> {
        if process.status.is_terminal() {
            return Ok(process);
        }

        let outcome = self
            .backend
            .exec(
                &process.sandbox_id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        inspect_script(&process),
                    ],
                    timeout_secs: 5,
                },
            )
            .await?;
        if outcome.exit_code != 0 {
            return Ok(process);
        }

        let inspected = parse_process_probe(&process, outcome.stdout.trim())?;
        if inspected != process {
            self.snapshots.upsert_process(&inspected).await?;
        }
        Ok(inspected)
    }

    async fn read_log_size(&self, sandbox_id: &SandboxId, path: &str) -> Result<u64> {
        let outcome = self
            .backend
            .exec(
                sandbox_id,
                ExecRequest {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        format!(
                            "path={}; if [ ! -f \"$path\" ]; then printf 0; else wc -c < \"$path\" | tr -d ' '; fi",
                            shell_escape(path)
                        ),
                    ],
                    timeout_secs: 5,
                },
            )
            .await?;
        let total = outcome.stdout.trim();
        if total.is_empty() {
            return Ok(0);
        }
        total.parse::<u64>().map_err(|err| {
            HyperboxError::ExecutionFailed(format!("invalid log size for {path}: {err}"))
        })
    }

    async fn cleanup_expired_processes(&self) -> Result<()> {
        let now = Utc::now();
        for process in self.snapshots.list_processes().await? {
            let expired = process
                .expires_at
                .is_some_and(|expires_at| expires_at <= now);
            if expired {
                self.purge_process(&process).await?;
            }
        }
        Ok(())
    }

    async fn run_ensure_commands(
        &self,
        sandbox_id: &SandboxId,
        ensure_commands: &[String],
        timeout_secs: u64,
    ) -> Result<()> {
        for ensure in ensure_commands {
            let marker = format!(".hyperbox/ensure/{:016x}.done", fnv1a64(ensure.as_bytes()));
            let script = format!(
                "set -e\nmkdir -p .hyperbox/ensure\nif [ ! -f \"{marker}\" ]; then\n{ensure}\ntouch \"{marker}\"\nfi"
            );
            let outcome = self
                .backend
                .exec(
                    sandbox_id,
                    ExecRequest {
                        command: vec!["/bin/sh".to_string(), "-lc".to_string(), script],
                        timeout_secs,
                    },
                )
                .await?;
            if outcome.exit_code != 0 {
                let stderr = outcome.stderr.trim();
                let message = if stderr.is_empty() {
                    format!("--ensure step failed with exit code {}", outcome.exit_code)
                } else {
                    format!(
                        "--ensure step failed with exit code {}: {stderr}",
                        outcome.exit_code
                    )
                };
                return Err(HyperboxError::ExecutionFailed(message));
            }
        }
        Ok(())
    }

    async fn purge_process(&self, process: &ProcessInfo) -> Result<()> {
        match self.backend.inspect(&process.sandbox_id).await {
            Ok(_) if process.destroy_sandbox_on_expiry => {
                self.backend.destroy(&process.sandbox_id).await?;
                self.sandboxes.lock().await.remove(&process.sandbox_id);
                self.snapshots
                    .clear_sandbox_binding(&process.sandbox_id)
                    .await?;
                self.snapshots
                    .remove_active_sandbox(&process.sandbox_id)
                    .await?;
            }
            Ok(_) => {
                let _ = self
                    .backend
                    .exec(
                        &process.sandbox_id,
                        ExecRequest {
                            command: vec![
                                "/bin/sh".to_string(),
                                "-lc".to_string(),
                                format!(
                                    "rm -rf {}",
                                    shell_escape(process_dir(&process.id).as_str())
                                ),
                            ],
                            timeout_secs: 5,
                        },
                    )
                    .await;
            }
            Err(HyperboxError::SandboxNotFound(_)) => {
                if process.destroy_sandbox_on_expiry {
                    self.sandboxes.lock().await.remove(&process.sandbox_id);
                    self.snapshots
                        .clear_sandbox_binding(&process.sandbox_id)
                        .await?;
                    self.snapshots
                        .remove_active_sandbox(&process.sandbox_id)
                        .await?;
                }
            }
            Err(err) => return Err(err),
        }
        self.snapshots.remove_process(&process.id).await
    }
}

fn process_dir(process_id: &ProcessId) -> String {
    format!(".hyperbox/processes/{}", process_id.0)
}

fn process_stdout_path(process_id: &ProcessId) -> String {
    format!("{}/stdout.log", process_dir(process_id))
}

fn process_stderr_path(process_id: &ProcessId) -> String {
    format!("{}/stderr.log", process_dir(process_id))
}

fn process_pid_path(process_id: &ProcessId) -> String {
    format!("{}/pid", process_dir(process_id))
}

fn process_exit_code_path(process_id: &ProcessId) -> String {
    format!("{}/exit_code", process_dir(process_id))
}

fn process_cancel_path(process_id: &ProcessId) -> String {
    format!("{}/cancelled", process_dir(process_id))
}

fn process_expiry_at(now: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    now + ChronoDuration::days(
        std::env::var("HYPERBOX_PROCESS_TTL_DAYS")
            .ok()
            .and_then(|raw| raw.parse::<i64>().ok())
            .filter(|days| *days > 0)
            .unwrap_or(7),
    )
}

fn process_lost_grace() -> ChronoDuration {
    ChronoDuration::seconds(5)
}

fn process_wait_poll_interval(elapsed: Duration) -> Duration {
    if elapsed < Duration::from_secs(1) {
        Duration::from_millis(25)
    } else {
        Duration::from_millis(100)
    }
}

fn process_launch_command(process: &ProcessInfo) -> Result<String> {
    let base = process_dir(&process.id);
    let command = shell_join(&process.command);
    Ok(format!(
        "set -eu; base={base}; mkdir -p \"$base\"; : > \"$base/stdout.log\"; : > \"$base/stderr.log\"; rm -f \"$base/exit_code\" \"$base/pid\" \"$base/cancelled\"; nohup /bin/sh -lc {wrapped} >> \"$base/stdout.log\" 2>> \"$base/stderr.log\" </dev/null & pid=$!; printf \"%s\" \"$pid\" > \"$base/pid\"",
        base = shell_escape(&base),
        wrapped = shell_escape(&format!(
            "export PYTHONUNBUFFERED=1; {command}; code=$?; printf '%s' \"$code\" > {exit_path}; exit \"$code\"",
            command = command,
            exit_path = shell_escape(&process_exit_code_path(&process.id)),
        )),
    ))
}

fn inspect_script(process: &ProcessInfo) -> String {
    let base = shell_escape(process_dir(&process.id).as_str());
    format!(
        "base={base}; if [ -f \"$base/exit_code\" ]; then code=''; IFS= read -r code < \"$base/exit_code\" || [ -n \"$code\" ]; if [ -f \"$base/cancelled\" ]; then printf 'cancelled:%s' \"$code\"; elif [ \"$code\" = '0' ]; then printf 'succeeded:%s' \"$code\"; else printf 'failed:%s' \"$code\"; fi; elif [ -f \"$base/pid\" ]; then pid=''; IFS= read -r pid < \"$base/pid\" || [ -n \"$pid\" ]; if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then printf 'running:%s' \"$pid\"; elif [ -f \"$base/cancelled\" ]; then printf 'cancelled:'; else printf 'lost'; fi; else printf 'starting'; fi",
    )
}

fn cancel_script(process: &ProcessInfo) -> String {
    let pid_path = shell_escape(&process_pid_path(&process.id));
    format!(
        "pid_file={pid_path}; if [ ! -f \"$pid_file\" ]; then exit 0; fi; pid=''; IFS= read -r pid < \"$pid_file\" || [ -n \"$pid\" ] || exit 0; \
         kill_descendants() {{ target=\"$1\"; if command -v pgrep >/dev/null 2>&1; then for child in $(pgrep -P \"$target\" 2>/dev/null || true); do kill_descendants \"$child\"; done; fi; kill -TERM \"$target\" 2>/dev/null || true; }}; \
         kill_descendants_kill() {{ target=\"$1\"; if command -v pgrep >/dev/null 2>&1; then for child in $(pgrep -P \"$target\" 2>/dev/null || true); do kill_descendants_kill \"$child\"; done; fi; kill -KILL \"$target\" 2>/dev/null || true; }}; \
         kill_descendants \"$pid\"; sleep 1; if kill -0 \"$pid\" 2>/dev/null; then kill_descendants_kill \"$pid\"; fi",
    )
}

fn parse_process_probe(process: &ProcessInfo, probe: &str) -> Result<ProcessInfo> {
    let mut updated = process.clone();
    if let Some(pid) = probe.strip_prefix("running:") {
        updated.status = ProcessStatus::Running;
        updated.backend_pid = pid.parse::<u32>().ok();
        updated.exit_code = None;
        updated.finished_at = None;
        updated.expires_at = None;
        return Ok(updated);
    }

    let now = Utc::now();
    if let Some(code) = probe.strip_prefix("succeeded:") {
        updated.status = ProcessStatus::Succeeded;
        updated.exit_code = code.parse::<i32>().ok();
        updated.finished_at.get_or_insert(now);
        updated.expires_at.get_or_insert(process_expiry_at(now));
        return Ok(updated);
    }
    if let Some(code) = probe.strip_prefix("failed:") {
        updated.status = ProcessStatus::Failed;
        updated.exit_code = code.parse::<i32>().ok();
        updated.finished_at.get_or_insert(now);
        updated.expires_at.get_or_insert(process_expiry_at(now));
        return Ok(updated);
    }
    if probe.starts_with("cancelled:") {
        updated.status = ProcessStatus::Cancelled;
        updated.exit_code = probe.split_once(':').and_then(|(_, value)| {
            if value.is_empty() {
                None
            } else {
                value.parse::<i32>().ok()
            }
        });
        updated.finished_at.get_or_insert(now);
        updated.expires_at.get_or_insert(process_expiry_at(now));
        return Ok(updated);
    }
    if probe == "lost" {
        if (now - updated.started_at) < process_lost_grace() {
            updated.status = ProcessStatus::Starting;
            updated.exit_code = None;
            updated.finished_at = None;
            updated.expires_at = None;
            return Ok(updated);
        }
        updated.status = ProcessStatus::Lost;
        updated.finished_at.get_or_insert(now);
        updated.expires_at.get_or_insert(process_expiry_at(now));
        return Ok(updated);
    }
    Ok(updated)
}

fn shell_join(command: &[String]) -> String {
    command
        .iter()
        .map(|part| shell_escape(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn derive_auto_run_affinity_name(
    template: &str,
    workspace_dir: Option<&str>,
    network: &hyperbox_core::NetworkMode,
) -> String {
    let mut scope = String::new();
    scope.push_str(template);
    scope.push('\n');
    scope.push_str(workspace_dir.unwrap_or(""));
    scope.push('\n');
    scope.push_str(match network {
        hyperbox_core::NetworkMode::None => "none",
        hyperbox_core::NetworkMode::Full => "full",
        hyperbox_core::NetworkMode::Allowlist(domains) => {
            if domains.is_empty() {
                "allowlist"
            } else {
                return format!(
                    "auto-{}-{:016x}",
                    sanitize_affinity_component(template, 18),
                    fnv1a64(
                        format!(
                            "{template}\n{}\nallowlist:{}",
                            workspace_dir.unwrap_or(""),
                            domains.to_strings().join(",")
                        )
                        .as_bytes()
                    )
                );
            }
        }
    });
    let hash = fnv1a64(scope.as_bytes());
    let template_slug = sanitize_affinity_component(template, 18);
    format!("auto-{template_slug}-{hash:016x}")
}

fn sanitize_affinity_component(raw: &str, max_len: usize) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| match ch {
            'a'..='z' | '0'..='9' => ch,
            'A'..='Z' => ch.to_ascii_lowercase(),
            _ => '-',
        })
        .collect::<String>();
    let trimmed = sanitized
        .trim_matches('-')
        .chars()
        .take(max_len)
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if trimmed.is_empty() {
        "sandbox".to_string()
    } else {
        trimmed
    }
}

fn is_affinity_absent_error(err: &HyperboxError) -> bool {
    let message = err.to_string();
    message.contains("affinity not found") || message.contains("has no active sandbox")
}

fn snapshot_artifact_path(snapshot_id: &SnapshotId) -> Result<PathBuf> {
    let root = if let Ok(value) = std::env::var("HYPERBOX_SNAPSHOT_ROOT") {
        PathBuf::from(value)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".hyperbox/snapshots")
    } else {
        std::env::temp_dir().join("hyperbox/snapshots")
    };
    std::fs::create_dir_all(&root)?;
    Ok(root.join(format!("{}.tar.gz", snapshot_id.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalBackend;
    use hyperbox_core::{ProcessDisposition, ProcessStatus, StreamName};
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;

    struct CountingBackend {
        inner: Arc<dyn SandboxBackend>,
        exec_calls: AtomicUsize,
    }

    impl CountingBackend {
        fn new(inner: Arc<dyn SandboxBackend>) -> Self {
            Self {
                inner,
                exec_calls: AtomicUsize::new(0),
            }
        }

        fn exec_calls(&self) -> usize {
            self.exec_calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl SandboxBackend for CountingBackend {
        fn validate_config(&self, config: &SandboxConfig) -> Result<()> {
            self.inner.validate_config(config)
        }

        async fn create(&self, config: SandboxConfig) -> Result<hyperbox_core::SandboxLease> {
            self.inner.create(config).await
        }

        async fn exec(&self, sandbox_id: &SandboxId, req: ExecRequest) -> Result<ExecOutcome> {
            self.exec_calls.fetch_add(1, Ordering::Relaxed);
            self.inner.exec(sandbox_id, req).await
        }

        async fn read_file(&self, sandbox_id: &SandboxId, path: &str) -> Result<FilePayload> {
            self.inner.read_file(sandbox_id, path).await
        }

        async fn write_file(&self, sandbox_id: &SandboxId, payload: FilePayload) -> Result<()> {
            self.inner.write_file(sandbox_id, payload).await
        }

        async fn destroy(&self, sandbox_id: &SandboxId) -> Result<()> {
            self.inner.destroy(sandbox_id).await
        }

        async fn inspect(&self, sandbox_id: &SandboxId) -> Result<SandboxInfo> {
            self.inner.inspect(sandbox_id).await
        }

        async fn create_snapshot(
            &self,
            sandbox_id: &SandboxId,
            snapshot_id: &SnapshotId,
            artifact_path: &Path,
        ) -> Result<()> {
            self.inner
                .create_snapshot(sandbox_id, snapshot_id, artifact_path)
                .await
        }

        async fn restore_snapshot(
            &self,
            snapshot_id: &SnapshotId,
            artifact_path: &Path,
            config: SandboxConfig,
        ) -> Result<hyperbox_core::SandboxLease> {
            self.inner
                .restore_snapshot(snapshot_id, artifact_path, config)
                .await
        }
    }

    fn unique_test_root(name: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{suffix}"))
    }

    #[tokio::test]
    async fn server_lifecycle_works() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-lifecycle-test",
        ))));
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

    #[tokio::test]
    async fn list_sandboxes_returns_active_sandbox_ids() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-list-test",
        ))));
        let server = HyperboxServer::new(backend);

        let info = server
            .create_sandbox(SandboxConfig {
                affinity_name: Some("list-test".to_string()),
                ..SandboxConfig::default()
            })
            .await
            .expect("create sandbox");

        let rows = server.list_sandboxes().await;
        assert!(rows.iter().any(|row| row.info.id == info.id));
        assert!(
            rows.iter()
                .any(|row| row.affinity_name.as_deref() == Some("list-test"))
        );
    }

    #[tokio::test]
    async fn restore_snapshot_fails_when_artifact_is_missing() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-restore-missing-artifact",
        ))));
        let snapshots = Arc::new(crate::InMemorySnapshotStore::default());
        let server = HyperboxServer::new_with_snapshots(backend, snapshots.clone());

        let info = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");
        let snapshot = snapshots
            .create_snapshot(
                &info.id,
                &SandboxConfig::default(),
                Some("no-artifact".to_string()),
            )
            .await
            .expect("create metadata only snapshot");

        let err = server
            .restore_snapshot(&snapshot.id)
            .await
            .expect_err("restore should fail when artifact is missing");
        assert!(err.to_string().contains("snapshot artifact missing"));
    }

    #[tokio::test]
    async fn managed_process_runs_and_persists_logs() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-run-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let process = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "printf hello && printf err >&2".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start process");

        let completed = server
            .wait_process(&process.id, 5)
            .await
            .expect("wait process");
        assert_eq!(completed.status, ProcessStatus::Succeeded);
        assert_eq!(completed.exit_code, Some(0));

        let stdout = server
            .read_process_log(&process.id, StreamName::Stdout, 0, 1024)
            .await
            .expect("read stdout");
        assert_eq!(stdout.contents, "hello");

        let stderr = server
            .read_process_log(&process.id, StreamName::Stderr, 0, 1024)
            .await
            .expect("read stderr");
        assert_eq!(stderr.contents, "err");
    }

    #[tokio::test]
    async fn start_process_launches_with_single_backend_exec() {
        let inner: Arc<dyn SandboxBackend> = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-launch-count-test",
        ))));
        let backend = Arc::new(CountingBackend::new(inner));
        let server = HyperboxServer::new(backend.clone());

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let process = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "sleep 30".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start process");

        assert_eq!(process.status, ProcessStatus::Running);
        assert_eq!(backend.exec_calls(), 1);
    }

    #[test]
    fn process_launch_command_sets_up_logs_inline() {
        let process_id = ProcessId::new();
        let process = ProcessInfo {
            id: process_id.clone(),
            sandbox_id: SandboxId::new(),
            requested_sandbox_id: None,
            disposition: ProcessDisposition::ReusedExisting,
            destroy_sandbox_on_expiry: false,
            command: vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "printf ok".to_string(),
            ],
            status: ProcessStatus::Starting,
            stdout_path: process_stdout_path(&process_id),
            stderr_path: process_stderr_path(&process_id),
            backend_pid: None,
            exit_code: None,
            started_at: Utc::now(),
            finished_at: None,
            expires_at: None,
        };

        let launch = process_launch_command(&process).expect("build launch command");
        assert!(launch.contains("mkdir -p"));
        assert!(launch.contains("nohup /bin/sh -lc"));
        assert!(launch.contains("stdout.log"));
        assert!(launch.contains("stderr.log"));
        assert!(launch.contains("exit_code"));
        assert!(!launch.contains("launch.sh"));
    }

    #[test]
    fn process_probe_scripts_use_shell_builtins_for_metadata_reads() {
        let process_id = ProcessId::new();
        let process = ProcessInfo {
            id: process_id.clone(),
            sandbox_id: SandboxId::new(),
            requested_sandbox_id: None,
            disposition: ProcessDisposition::ReusedExisting,
            destroy_sandbox_on_expiry: false,
            command: vec!["sleep".to_string(), "1".to_string()],
            status: ProcessStatus::Running,
            stdout_path: process_stdout_path(&process_id),
            stderr_path: process_stderr_path(&process_id),
            backend_pid: Some(42),
            exit_code: None,
            started_at: Utc::now(),
            finished_at: None,
            expires_at: None,
        };

        let inspect = inspect_script(&process);
        assert!(inspect.contains("read -r code"));
        assert!(inspect.contains("read -r pid"));
        assert!(!inspect.contains("cat \"$base/exit_code\""));
        assert!(!inspect.contains("cat \"$base/pid\""));

        let cancel = cancel_script(&process);
        assert!(cancel.contains("read -r pid"));
        assert!(!cancel.contains("cat \"$pid_file\""));
    }

    #[test]
    fn lost_probe_stays_non_terminal_within_grace_period() {
        let now = Utc::now();
        let process = ProcessInfo {
            id: ProcessId::new(),
            sandbox_id: SandboxId::new(),
            requested_sandbox_id: None,
            disposition: ProcessDisposition::ReusedExisting,
            destroy_sandbox_on_expiry: false,
            command: vec!["sleep".to_string(), "1".to_string()],
            status: ProcessStatus::Running,
            stdout_path: "stdout.log".to_string(),
            stderr_path: "stderr.log".to_string(),
            backend_pid: Some(123),
            exit_code: None,
            started_at: now - ChronoDuration::seconds(2),
            finished_at: None,
            expires_at: None,
        };

        let parsed = parse_process_probe(&process, "lost").expect("parse lost probe");
        assert_eq!(parsed.status, ProcessStatus::Starting);
        assert!(parsed.finished_at.is_none());
    }

    #[test]
    fn wait_process_polls_more_aggressively_for_short_lived_work() {
        assert_eq!(
            process_wait_poll_interval(Duration::from_millis(0)),
            Duration::from_millis(25)
        );
        assert_eq!(
            process_wait_poll_interval(Duration::from_millis(999)),
            Duration::from_millis(25)
        );
        assert_eq!(
            process_wait_poll_interval(Duration::from_secs(1)),
            Duration::from_millis(100)
        );
    }

    #[tokio::test]
    async fn managed_process_exposes_logs_while_running() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-live-log-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let process = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "python3 -c 'import time; print(\"start\"); time.sleep(1); print(\"done\")'"
                        .to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start process");

        tokio::time::sleep(Duration::from_millis(250)).await;

        let stdout = server
            .read_process_log(&process.id, StreamName::Stdout, 0, 1024)
            .await
            .expect("read stdout while running");
        assert!(
            stdout.contents.contains("start"),
            "stdout should be visible before process exit, got {:?}",
            stdout.contents
        );

        let completed = server
            .wait_process(&process.id, 5)
            .await
            .expect("wait process");
        assert_eq!(completed.status, ProcessStatus::Succeeded);
    }

    #[tokio::test]
    async fn managed_process_rejects_second_process_in_same_sandbox() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-busy-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let first = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "sleep 30".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start first process");
        assert_eq!(first.status, ProcessStatus::Running);

        let err = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "echo second".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect_err("second process should be rejected");
        assert!(
            err.to_string()
                .contains("already has a running managed process")
        );
    }

    #[tokio::test]
    async fn managed_process_can_be_cancelled() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-cancel-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let process = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "sleep 30".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start process");

        let cancelled = server
            .cancel_process(&process.id)
            .await
            .expect("cancel process");
        assert_eq!(cancelled.status, ProcessStatus::Cancelled);
        assert!(cancelled.finished_at.is_some());
    }

    #[tokio::test]
    async fn prepare_run_sandbox_overflows_to_new_sandbox_when_busy() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-overflow-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig {
                affinity_name: Some("overflow-test".to_string()),
                ..SandboxConfig::default()
            })
            .await
            .expect("create sandbox");

        let first = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "sleep 30".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start first process");
        assert_eq!(first.status, ProcessStatus::Running);

        let prepared = server
            .prepare_run_sandbox(&sandbox.id, Some(SandboxConfig::default()))
            .await
            .expect("busy sandbox should overflow");
        assert_eq!(prepared.disposition, ProcessDisposition::CreatedDueToBusy);
        assert_eq!(prepared.requested_sandbox_id, Some(sandbox.id.clone()));
        assert_ne!(prepared.info.id, sandbox.id);
    }

    #[tokio::test]
    async fn expired_process_reclaims_owned_sandbox() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-expiry-owned-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let process = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "printf owned-cleanup".to_string(),
                ],
                None,
                ProcessDisposition::CreatedNew,
                true,
            )
            .await
            .expect("start process");

        let completed = server
            .wait_process(&process.id, 5)
            .await
            .expect("wait process");
        assert_eq!(completed.status, ProcessStatus::Succeeded);

        let mut expired = completed.clone();
        expired.expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        server
            .snapshots
            .upsert_process(&expired)
            .await
            .expect("expire process");

        assert!(
            server
                .list_processes()
                .await
                .expect("list processes")
                .is_empty()
        );
        let inspect_err = server
            .inspect(&sandbox.id)
            .await
            .expect_err("owned sandbox should be reclaimed");
        assert!(matches!(inspect_err, HyperboxError::SandboxNotFound(_)));
    }

    #[tokio::test]
    async fn expired_process_keeps_unowned_sandbox() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-process-expiry-unowned-test",
        ))));
        let server = HyperboxServer::new(backend);

        let sandbox = server
            .create_sandbox(SandboxConfig::default())
            .await
            .expect("create sandbox");

        let process = server
            .start_process(
                &sandbox.id,
                vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "printf keep-sandbox".to_string(),
                ],
                None,
                ProcessDisposition::ReusedExisting,
                false,
            )
            .await
            .expect("start process");

        let completed = server
            .wait_process(&process.id, 5)
            .await
            .expect("wait process");
        assert_eq!(completed.status, ProcessStatus::Succeeded);

        let mut expired = completed.clone();
        expired.expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        server
            .snapshots
            .upsert_process(&expired)
            .await
            .expect("expire process");

        assert!(
            server
                .list_processes()
                .await
                .expect("list processes")
                .is_empty()
        );
        let inspected = server
            .inspect(&sandbox.id)
            .await
            .expect("sandbox should remain");
        assert_eq!(inspected.id, sandbox.id);
    }

    #[tokio::test]
    async fn start_run_reuses_auto_session_and_applies_setup_steps() {
        let backend = Arc::new(LocalBackend::new(Some(unique_test_root(
            "hyperbox-server-start-run-auto-session-test",
        ))));
        let server = HyperboxServer::new(backend);

        let first = server
            .start_run(StartRunRequest {
                sandbox_id: None,
                affinity_name: None,
                create_config: Some(SandboxConfig::default()),
                reuse_auto_session: true,
                ensure_commands: vec!["printf ensured > ensured.txt".to_string()],
                writes: vec![("input.txt".to_string(), b"seed".to_vec())],
                command: "cat input.txt && printf ':' && cat ensured.txt".to_string(),
                destroy_sandbox_on_expiry: false,
            })
            .await
            .expect("start first run");
        assert!(first.session_created);
        assert!(first.session_name.is_some());
        assert_eq!(first.process.status, ProcessStatus::Running);

        let first_done = server
            .wait_process(&first.process.id, 5)
            .await
            .expect("wait first run");
        assert_eq!(first_done.status, ProcessStatus::Succeeded);
        let first_stdout = server
            .read_process_log(&first.process.id, StreamName::Stdout, 0, 1024)
            .await
            .expect("read first stdout");
        assert_eq!(first_stdout.contents, "seed:ensured");

        let second = server
            .start_run(StartRunRequest {
                sandbox_id: None,
                affinity_name: None,
                create_config: Some(SandboxConfig::default()),
                reuse_auto_session: true,
                ensure_commands: vec!["printf ensured > ensured.txt".to_string()],
                writes: vec![],
                command: "cat input.txt && printf ':' && cat ensured.txt".to_string(),
                destroy_sandbox_on_expiry: false,
            })
            .await
            .expect("start second run");
        assert!(!second.session_created);
        assert_eq!(second.sandbox.id, first.sandbox.id);

        let second_done = server
            .wait_process(&second.process.id, 5)
            .await
            .expect("wait second run");
        assert_eq!(second_done.status, ProcessStatus::Succeeded);
        let second_stdout = server
            .read_process_log(&second.process.id, StreamName::Stdout, 0, 1024)
            .await
            .expect("read second stdout");
        assert_eq!(second_stdout.contents, "seed:ensured");
    }
}

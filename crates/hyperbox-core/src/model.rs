use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SandboxId(pub Uuid);

impl SandboxId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SandboxId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Provisioning,
    Ready,
    Busy,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxInfo {
    pub id: SandboxId,
    pub template: String,
    pub state: SandboxState,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    Stdout { chunk: String },
    Stderr { chunk: String },
    Exit { code: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NetworkMode, SandboxConfig};

    #[test]
    fn default_config_is_safe() {
        let cfg = SandboxConfig::default();
        assert!(matches!(cfg.network, NetworkMode::None));
        assert_eq!(cfg.template, "python:3.12");
    }

    #[test]
    fn sandbox_id_default_generates_id() {
        let id = SandboxId::default();
        assert_ne!(id.0, Uuid::nil());
    }
}

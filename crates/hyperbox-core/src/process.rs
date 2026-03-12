use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{ExecOutcome, ExecRequest, SandboxId};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamName {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamChunk {
    pub stream: StreamName,
    pub chunk: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecStatus {
    Success,
    NonZeroExit,
    Timeout,
    StartFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessId(pub Uuid);

impl ProcessId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ProcessId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessDisposition {
    ReusedExisting,
    CreatedNew,
    CreatedDueToBusy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Starting,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
}

impl ProcessStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost
        )
    }

    pub fn can_transition_to(&self, next: Self) -> bool {
        match self {
            Self::Starting => matches!(next, Self::Running | Self::Failed),
            Self::Running => matches!(
                next,
                Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost
            ),
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Lost => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub id: ProcessId,
    pub sandbox_id: SandboxId,
    pub requested_sandbox_id: Option<SandboxId>,
    pub disposition: ProcessDisposition,
    pub destroy_sandbox_on_expiry: bool,
    pub command: Vec<String>,
    pub status: ProcessStatus,
    pub stdout_path: String,
    pub stderr_path: String,
    pub backend_pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessLogRead {
    pub stream: StreamName,
    pub offset: u64,
    pub next_offset: u64,
    pub eof: bool,
    pub contents: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecTrace {
    pub request: ExecRequest,
    pub outcome: ExecOutcome,
    pub status: ExecStatus,
    pub stream: Vec<StreamChunk>,
}

impl ExecTrace {
    pub fn from_outcome(request: ExecRequest, outcome: ExecOutcome) -> Self {
        let status = if outcome.exit_code == 0 {
            ExecStatus::Success
        } else {
            ExecStatus::NonZeroExit
        };

        Self {
            request,
            outcome,
            status,
            stream: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_nonzero_exit() {
        let trace = ExecTrace::from_outcome(
            ExecRequest {
                command: vec!["false".to_string()],
                timeout_secs: 1,
            },
            ExecOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 1,
            },
        );

        assert!(matches!(trace.status, ExecStatus::NonZeroExit));
    }

    #[test]
    fn process_status_terminal_and_transition_rules() {
        assert!(ProcessStatus::Succeeded.is_terminal());
        assert!(!ProcessStatus::Running.is_terminal());
        assert!(ProcessStatus::Starting.can_transition_to(ProcessStatus::Running));
        assert!(!ProcessStatus::Succeeded.can_transition_to(ProcessStatus::Running));
    }
}

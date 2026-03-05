use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{ExecOutcome, ExecRequest};

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
}

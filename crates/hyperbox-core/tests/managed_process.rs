use chrono::Utc;
use hyperbox_core::{ProcessDisposition, ProcessId, ProcessInfo, ProcessStatus, SandboxId};

#[test]
fn running_process_is_not_terminal() {
    let process = ProcessInfo {
        id: ProcessId::new(),
        sandbox_id: SandboxId::new(),
        requested_sandbox_id: None,
        disposition: ProcessDisposition::CreatedNew,
        destroy_sandbox_on_expiry: false,
        command: vec!["python".to_string(), "train.py".to_string()],
        status: ProcessStatus::Running,
        stdout_path: ".hyperbox/processes/stdout.log".to_string(),
        stderr_path: ".hyperbox/processes/stderr.log".to_string(),
        backend_pid: Some(123),
        exit_code: None,
        started_at: Utc::now(),
        finished_at: None,
        expires_at: None,
    };

    assert!(!process.status.is_terminal());
}

#[test]
fn disallows_terminal_to_running_transition() {
    assert!(!ProcessStatus::Succeeded.can_transition_to(ProcessStatus::Running));
    assert!(ProcessStatus::Starting.can_transition_to(ProcessStatus::Running));
}

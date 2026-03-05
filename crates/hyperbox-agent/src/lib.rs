use hyperbox_core::{ExecOutcome, ExecRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentRequest {
    Ping,
    Exec { request: ExecRequest },
    ReadFile { path: String },
    WriteFile { path: String, bytes: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentResponse {
    Pong,
    ExecResult { outcome: ExecOutcome },
    FileData { bytes: Vec<u8> },
    Ok,
    Error { message: String },
}

pub fn encode_request(request: &AgentRequest) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(request)
}

pub fn decode_request(bytes: &[u8]) -> Result<AgentRequest, serde_json::Error> {
    serde_json::from_slice(bytes)
}

pub fn encode_response(response: &AgentResponse) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(response)
}

pub fn decode_response(bytes: &[u8]) -> Result<AgentResponse, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrip_exec() {
        let req = AgentRequest::Exec {
            request: ExecRequest {
                command: vec!["python".to_string(), "-V".to_string()],
                timeout_secs: 5,
            },
        };

        let bytes = encode_request(&req).expect("serialize request");
        let got = decode_request(&bytes).expect("deserialize request");
        assert_eq!(req, got);
    }
}

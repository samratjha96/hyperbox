use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use uuid::Uuid;

fn hyperbox_bin() -> &'static str {
    env!("CARGO_BIN_EXE_hyperbox")
}

fn have_python3() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{}-{}", prefix, Uuid::new_v4()))
}

fn write_mock_container_script(path: &Path) {
    let script = r#"#!/usr/bin/env python3
import json
import os
import subprocess
import sys

STATE_PATH = os.environ["MOCK_CONTAINER_STATE"]
WORKDIR_IN_CONTAINER = "/workspace"


def load_state():
    if not os.path.exists(STATE_PATH):
        return {}
    with open(STATE_PATH, "r", encoding="utf-8") as f:
        return json.load(f)


def save_state(state):
    with open(STATE_PATH, "w", encoding="utf-8") as f:
        json.dump(state, f)


def map_arg(arg, workspace):
    if arg == WORKDIR_IN_CONTAINER:
        return workspace
    prefix = WORKDIR_IN_CONTAINER + "/"
    if arg.startswith(prefix):
        return os.path.join(workspace, arg[len(prefix):])
    return arg


def cmd_run(argv):
    name = None
    volume = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--name":
            name = argv[i + 1]
            i += 2
            continue
        if arg == "--volume":
            volume = argv[i + 1]
            i += 2
            continue
        if arg in ("--detach",):
            i += 1
            continue
        if arg in ("--network", "--workdir", "--cpus", "--memory"):
            i += 2
            continue
        break

    if not name or not volume:
        print("missing --name or --volume", file=sys.stderr)
        return 1

    workspace = volume.split(":", 1)[0]
    state = load_state()
    state[name] = {"workspace": workspace}
    save_state(state)
    print(name)
    return 0


def cmd_exec(argv):
    interactive = False
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--interactive":
            interactive = True
            i += 1
            continue
        if arg == "--workdir":
            i += 2
            continue
        break

    if i >= len(argv):
        print("missing container name", file=sys.stderr)
        return 1

    name = argv[i]
    cmd = argv[i + 1 :]
    state = load_state()
    if name not in state:
        print("container not found", file=sys.stderr)
        return 1
    workspace = state[name]["workspace"]

    mapped = [map_arg(x, workspace) for x in cmd]
    stdin_data = sys.stdin.buffer.read() if interactive else None

    proc = subprocess.run(
        mapped,
        input=stdin_data,
        cwd=workspace,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    sys.stdout.buffer.write(proc.stdout)
    sys.stderr.buffer.write(proc.stderr)
    return proc.returncode


def cmd_delete(argv):
    name = argv[-1]
    state = load_state()
    state.pop(name, None)
    save_state(state)
    return 0


def main():
    if len(sys.argv) < 2:
        print("missing command", file=sys.stderr)
        return 1
    command = sys.argv[1]
    argv = sys.argv[2:]

    if command == "run":
        return cmd_run(argv)
    if command == "exec":
        return cmd_exec(argv)
    if command == "delete":
        return cmd_delete(argv)

    print(f"unsupported command: {command}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
"#;

    fs::write(path, script).expect("write mock container script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).expect("script metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod mock container script");
    }
}

fn send_request<W: Write>(stdin: &mut W, request: Value) {
    let line = serde_json::to_string(&request).expect("serialize request");
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .expect("write request");
    stdin.flush().expect("flush request");
}

fn read_response<R: BufRead>(reader: &mut R) -> Value {
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("read response line");
    assert!(read > 0, "helper closed stdout unexpectedly");
    serde_json::from_str(&line).expect("parse response")
}

#[test]
fn apple_helper_supports_protocol_roundtrip_with_mock_container() {
    if !have_python3() {
        return;
    }

    let root = unique_temp_dir("hyperbox-apple-helper-test");
    let workspace = root.join("workspace");
    let state_root = root.join("state");
    let state_file = root.join("mock-container-state.json");
    let script_path = root.join("mock-container.py");

    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&state_root).expect("create state root");
    fs::write(&state_file, "{}\n").expect("write initial state file");
    write_mock_container_script(&script_path);

    let mut child = Command::new(hyperbox_bin())
        .args([
            "apple-helper",
            "--container-bin",
            script_path.to_str().expect("script path utf8"),
            "--state-root",
            state_root.to_str().expect("state root utf8"),
        ])
        .env(
            "MOCK_CONTAINER_STATE",
            state_file.to_str().expect("state file utf8"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn apple helper");

    let mut stdin = child.stdin.take().expect("helper stdin");
    let stdout = child.stdout.take().expect("helper stdout");
    let mut stdout_reader = BufReader::new(stdout);

    let sandbox_id = Uuid::new_v4().to_string();

    send_request(
        &mut stdin,
        json!({
            "op": "create",
            "sandbox_id": sandbox_id,
            "template": "python:3.12",
            "workspace_dir": workspace,
            "runtime": "containerization"
        }),
    );
    let create_response = read_response(&mut stdout_reader);
    assert_eq!(create_response["op"], "ack");

    send_request(
        &mut stdin,
        json!({
            "op": "write",
            "sandbox_id": sandbox_id,
            "path": "input.txt",
            "bytes_b64": BASE64.encode("hello-helper".as_bytes())
        }),
    );
    let write_response = read_response(&mut stdout_reader);
    assert_eq!(write_response["op"], "ack");

    send_request(
        &mut stdin,
        json!({
            "op": "exec",
            "sandbox_id": sandbox_id,
            "command": ["/bin/sh", "-lc", "cat input.txt > output.txt && printf done"],
            "timeout_secs": 30
        }),
    );
    let exec_response = read_response(&mut stdout_reader);
    assert_eq!(exec_response["op"], "exec");
    assert_eq!(exec_response["exit_code"], 0);
    assert_eq!(exec_response["stdout"], "done");

    send_request(
        &mut stdin,
        json!({
            "op": "read",
            "sandbox_id": sandbox_id,
            "path": "output.txt"
        }),
    );
    let read_file_response = read_response(&mut stdout_reader);
    assert_eq!(read_file_response["op"], "read");
    let bytes = BASE64
        .decode(
            read_file_response["bytes_b64"]
                .as_str()
                .expect("bytes_b64 string")
                .as_bytes(),
        )
        .expect("decode read bytes");
    assert_eq!(String::from_utf8_lossy(&bytes), "hello-helper");

    send_request(
        &mut stdin,
        json!({
            "op": "destroy",
            "sandbox_id": sandbox_id
        }),
    );
    let destroy_response = read_response(&mut stdout_reader);
    assert_eq!(destroy_response["op"], "ack");

    drop(stdin);

    let output = child.wait_with_output().expect("wait for helper exit");
    assert!(
        output.status.success(),
        "helper stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_dir_all(&root);
}

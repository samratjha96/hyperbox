use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

fn hyperbox_bin() -> &'static str {
    env!("CARGO_BIN_EXE_hyperbox")
}

fn spawn_server() -> Option<(Child, String)> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let addr = listener.local_addr().ok()?;
    drop(listener);

    let mut server = Command::new(hyperbox_bin())
        .args(["serve", "--addr", &addr.to_string()])
        .env("HYPERBOX_BACKEND", "local")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let url = format!("http://{}", addr);
    for _ in 0..30 {
        let probe = Command::new(hyperbox_bin())
            .args(["--server-url", &url, "templates"])
            .output()
            .expect("probe templates");
        if probe.status.success() {
            return Some((server, url));
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = server.kill();
    None
}

#[test]
fn templates_lists_python() {
    let Some((mut server, url)) = spawn_server() else {
        return;
    };
    let out = Command::new(hyperbox_bin())
        .args(["--server-url", &url, "templates"])
        .output()
        .expect("run templates");

    let _ = server.kill();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("python:3.12"));
}

#[test]
fn run_supports_write_and_read() {
    let Some((mut server, url)) = spawn_server() else {
        return;
    };
    let out = Command::new(hyperbox_bin())
        .args([
            "--server-url",
            &url,
            "run",
            "--template",
            "python:3.12",
            "--cmd",
            "cat input.txt > output.txt",
            "--write",
            "input.txt=hello-hyperbox",
            "--read",
            "output.txt",
            "--json",
        ])
        .output()
        .expect("run command");

    let _ = server.kill();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hello-hyperbox"));
}

#[test]
fn probe_outputs_json() {
    let Some((mut server, url)) = spawn_server() else {
        return;
    };
    let out = Command::new(hyperbox_bin())
        .args(["--server-url", &url, "probe"])
        .output()
        .expect("run probe");

    let _ = server.kill();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('"'));
}

#[test]
fn run_reuses_session_by_default() {
    let Some((mut server, url)) = spawn_server() else {
        return;
    };

    let first = Command::new(hyperbox_bin())
        .args([
            "--server-url",
            &url,
            "run",
            "--template",
            "python:3.12",
            "--cmd",
            "echo reusable-state > .hb_reuse_test",
        ])
        .output()
        .expect("run first command");
    assert!(first.status.success());

    let second = Command::new(hyperbox_bin())
        .args([
            "--server-url",
            &url,
            "run",
            "--template",
            "python:3.12",
            "--cmd",
            "cat .hb_reuse_test",
        ])
        .output()
        .expect("run second command");

    let _ = server.kill();
    assert!(second.status.success());
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(stdout.contains("reusable-state"));
}

#[test]
fn run_ensure_executes_once_per_reused_session() {
    let Some((mut server, url)) = spawn_server() else {
        return;
    };

    let ensure_cmd = "test ! -f .hb_once && echo ready > .hb_once";

    let first = Command::new(hyperbox_bin())
        .args([
            "--server-url",
            &url,
            "run",
            "--template",
            "python:3.12",
            "--ensure",
            ensure_cmd,
            "--cmd",
            "cat .hb_once",
        ])
        .output()
        .expect("run first ensure command");
    assert!(first.status.success());
    assert!(String::from_utf8_lossy(&first.stdout).contains("ready"));

    let second = Command::new(hyperbox_bin())
        .args([
            "--server-url",
            &url,
            "run",
            "--template",
            "python:3.12",
            "--ensure",
            ensure_cmd,
            "--cmd",
            "cat .hb_once",
        ])
        .output()
        .expect("run second ensure command");

    let _ = server.kill();
    assert!(second.status.success());
    assert!(String::from_utf8_lossy(&second.stdout).contains("ready"));
}

#[test]
fn list_shows_created_sandbox() {
    let Some((mut server, url)) = spawn_server() else {
        return;
    };

    let created = Command::new(hyperbox_bin())
        .args(["--server-url", &url, "create", "--template", "python:3.12"])
        .output()
        .expect("create sandbox");
    assert!(created.status.success());
    let sandbox_id = String::from_utf8_lossy(&created.stdout).trim().to_string();
    assert!(!sandbox_id.is_empty());

    let listed = Command::new(hyperbox_bin())
        .args(["--server-url", &url, "list"])
        .output()
        .expect("list sandboxes");

    let _ = server.kill();
    assert!(listed.status.success());
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains(&sandbox_id));
}

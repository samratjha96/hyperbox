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

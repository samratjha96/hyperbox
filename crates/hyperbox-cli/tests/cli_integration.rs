use std::process::Command;

fn hyperbox_bin() -> &'static str {
    env!("CARGO_BIN_EXE_hyperbox")
}

#[test]
fn templates_lists_python() {
    let out = Command::new(hyperbox_bin())
        .arg("templates")
        .output()
        .expect("run templates");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("python:3.12"));
}

#[test]
fn run_supports_write_and_read() {
    let out = Command::new(hyperbox_bin())
        .args([
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

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hello-hyperbox"));
}

#[test]
fn probe_outputs_json() {
    let out = Command::new(hyperbox_bin())
        .arg("probe")
        .output()
        .expect("run probe");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('"'));
}

use std::time::Duration;

use hyperbox_core::{ExecRequest, ProcessDisposition, ProcessStatus, SandboxConfig, StreamName};
use hyperbox_server::{GrpcControlClient, serve_grpc};

#[tokio::test]
async fn grpc_roundtrip_lifecycle() {
    unsafe { std::env::set_var("HYPERBOX_BACKEND", "local") };

    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(err) => panic!("bind ephemeral port: {err}"),
    };
    let addr = listener.local_addr().expect("read local addr");
    drop(listener);

    let server = tokio::spawn(async move {
        serve_grpc(addr).await.expect("serve grpc");
    });

    tokio::time::sleep(Duration::from_millis(250)).await;

    let endpoint = format!("http://{}", addr);
    let mut client = GrpcControlClient::connect(endpoint)
        .await
        .expect("connect grpc client");

    let sandbox = client
        .create_sandbox(SandboxConfig::default())
        .await
        .expect("create sandbox");

    client
        .write_file(&sandbox.id, "input.txt".to_string(), b"grpc-ok".to_vec())
        .await
        .expect("write file");

    let out = client
        .exec(
            &sandbox.id,
            ExecRequest {
                command: vec![
                    "/bin/sh".to_string(),
                    "-lc".to_string(),
                    "cat input.txt".to_string(),
                ],
                timeout_secs: 2,
            },
        )
        .await
        .expect("exec command");

    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("grpc-ok"));

    let bytes = client
        .read_file(&sandbox.id, "input.txt".to_string())
        .await
        .expect("read file");
    assert_eq!(String::from_utf8_lossy(&bytes), "grpc-ok");

    client
        .destroy_sandbox(&sandbox.id)
        .await
        .expect("destroy sandbox");

    server.abort();
}

#[tokio::test]
async fn grpc_managed_process_lifecycle() {
    unsafe { std::env::set_var("HYPERBOX_BACKEND", "local") };

    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(err) => panic!("bind ephemeral port: {err}"),
    };
    let addr = listener.local_addr().expect("read local addr");
    drop(listener);

    let server = tokio::spawn(async move {
        serve_grpc(addr).await.expect("serve grpc");
    });

    tokio::time::sleep(Duration::from_millis(250)).await;

    let endpoint = format!("http://{}", addr);
    let mut client = GrpcControlClient::connect(endpoint)
        .await
        .expect("connect grpc client");

    let sandbox = client
        .create_sandbox(SandboxConfig::default())
        .await
        .expect("create sandbox");

    let process = client
        .start_process(
            &sandbox.id,
            vec![
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "printf grpc-managed && printf err >&2".to_string(),
            ],
            None,
            ProcessDisposition::ReusedExisting,
        )
        .await
        .expect("start process");

    let completed = client
        .wait_process(&process.id, 5)
        .await
        .expect("wait process");
    assert_eq!(completed.status, ProcessStatus::Succeeded);

    let stdout = client
        .read_process_log(&process.id, StreamName::Stdout, 0, 1024)
        .await
        .expect("read stdout");
    assert_eq!(stdout.contents, "grpc-managed");

    let stderr = client
        .read_process_log(&process.id, StreamName::Stderr, 0, 1024)
        .await
        .expect("read stderr");
    assert_eq!(stderr.contents, "err");

    let listed = client.list_processes().await.expect("list processes");
    assert!(listed.iter().any(|entry| entry.id == process.id));

    client
        .destroy_sandbox(&sandbox.id)
        .await
        .expect("destroy sandbox");

    server.abort();
}

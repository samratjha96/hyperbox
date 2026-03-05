#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let addr: std::net::SocketAddr = std::env::var("HYPERBOX_SERVER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50051".to_string())
        .parse()?;

    tracing::info!(%addr, "starting hyperbox gRPC server");
    hyperbox_server::serve_grpc(addr).await
}

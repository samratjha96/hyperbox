#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("hyperbox_server=info,hyperbox_server::grpc=info")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let addr: std::net::SocketAddr = std::env::var("HYPERBOX_SERVER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50051".to_string())
        .parse()?;

    tracing::info!(%addr, "starting hyperbox gRPC server");
    hyperbox_server::serve_grpc(addr).await
}

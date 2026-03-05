#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let addr: std::net::SocketAddr = std::env::var("HYPERBOX_AGENT_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:60061".to_string())
        .parse()?;

    let root = std::env::var("HYPERBOX_AGENT_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("hyperbox-agentd"));

    tracing::info!(%addr, root=?root, "starting hyperbox agent daemon");
    hyperbox_agent::serve_agent(addr, root).await
}

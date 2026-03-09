use std::net::SocketAddr;

use hyperbox_core::NetworkMode;
use hyperbox_network::DnsAllowlistProxy;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let listen: SocketAddr = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:53535".to_string())
        .parse()?;
    let upstream: SocketAddr = args
        .next()
        .unwrap_or_else(|| "1.1.1.1:53".to_string())
        .parse()?;
    let allowlist: Vec<String> = args.collect();

    if allowlist.is_empty() {
        eprintln!("usage: dns_proxy_demo <listen> <upstream> <allow1> [allow2 ...]");
        std::process::exit(2);
    }

    println!(
        "starting dns proxy on {listen}, upstream {upstream}, allowlist {:?}",
        allowlist
    );

    let proxy = DnsAllowlistProxy::new(NetworkMode::Allowlist(allowlist), upstream);
    proxy
        .serve(listen, |resolved| {
            if !resolved.is_empty() {
                println!("resolved_ips={:?}", resolved);
            }
        })
        .await
}

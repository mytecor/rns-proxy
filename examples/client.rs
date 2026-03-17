//! Minimal RNS SOCKS5 proxy client (local SOCKS5 proxy).
//!
//! Usage:
//!   cargo run --example client -- --server <32-hex-char-hash>
//!   cargo run --example client -- --server <hash> --port 9050
//!   RUST_LOG=debug cargo run --example client -- --server <hash>
//!
//! Then use the proxy:
//!   curl --socks5 127.0.0.1:1080 https://example.com

use std::env;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    let args: Vec<String> = env::args().collect();

    let server_hex = args
        .windows(2)
        .find(|w| w[0] == "--server")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| {
            eprintln!("Usage: cargo run --example client -- --server <32-hex-char-hash> [--port <port>]");
            std::process::exit(1);
        });

    let port: u16 = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(1080);

    eprintln!("Starting RNS SOCKS5 proxy client...");
    eprintln!("  Server: {}", server_hex);
    eprintln!("  SOCKS5: 127.0.0.1:{}", port);
    eprintln!("Make sure rnsd is running (pip install rns && rnsd)");
    eprintln!();

    rns_proxy::client::run_client(&server_hex, port).await;
}

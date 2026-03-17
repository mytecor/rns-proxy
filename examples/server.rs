//! Minimal RNS SOCKS5 proxy server (exit node).
//!
//! Usage:
//!   cargo run --example server
//!   RUST_LOG=debug cargo run --example server
//!
//! Prerequisites:
//!   pip install rns && rnsd
//!
//! The server will print its destination hash on startup.
//! Pass this hash to the client example with `--server <hash>`.

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    eprintln!("Starting RNS SOCKS5 proxy server...");
    eprintln!("Make sure rnsd is running (pip install rns && rnsd)");
    eprintln!();

    rns_proxy::server::run_server().await;
}

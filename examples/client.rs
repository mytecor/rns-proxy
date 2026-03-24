//! Minimal RNS SOCKS5 proxy client (local SOCKS5 proxy).
//!
//! Usage:
//!   cargo run --example client -- -d <32-hex-char-hash>
//!   cargo run --example client -- -d <hash> -l 0.0.0.0:1080
//!   RUST_LOG=debug cargo run --example client -- -d <hash>
//!
//! Then use the proxy:
//!   curl --socks5 127.0.0.1:1080 https://example.com

use clap::Parser;
use rns_proxy::cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse_from(
        ["rns-proxy".to_string(), "client".to_string()]
            .into_iter()
            .chain(std::env::args().skip(1)),
    );

    let log_level = if cli.debug { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp_secs()
        .init();

    match cli.command {
        Commands::Client {
            destination,
            listen,
        } => {
            eprintln!("Starting RNS SOCKS5 proxy client...");
            eprintln!("  Destination: {}", destination);
            eprintln!("  SOCKS5: {}", listen);
            eprintln!("Make sure rnsd is running (pip install rns && rnsd)");
            eprintln!();
            rns_proxy::client::run_client(&destination, &listen).await;
        }
        _ => unreachable!(),
    }
}

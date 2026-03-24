//! Minimal RNS SOCKS5 proxy server (exit node).
//!
//! Usage:
//!   cargo run --example server
//!   cargo run --example server -- --identity-file /path/to/key
//!   RUST_LOG=debug cargo run --example server
//!
//! Prerequisites:
//!   pip install rns && rnsd
//!
//! The server will print its destination hash on startup.
//! Pass this hash to the client example with `-d <hash>`.
//!
//! Identity is persisted to `~/.reticulum/rns_proxy_identity` by default,
//! so the server address stays the same across restarts.

use clap::Parser;
use rns_proxy::cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse_from(
        ["rns-proxy".to_string(), "server".to_string()]
            .into_iter()
            .chain(std::env::args().skip(1)),
    );

    let log_level = if cli.debug { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp_secs()
        .init();

    match cli.command {
        Commands::Server { identity_file } => {
            eprintln!("Starting RNS SOCKS5 proxy server...");
            eprintln!("Make sure rnsd is running (pip install rns && rnsd)");
            eprintln!();
            rns_proxy::server::run_server(identity_file.as_deref()).await;
        }
        _ => unreachable!(),
    }
}

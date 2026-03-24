//! RNS SOCKS5 Proxy Service
//!
//! A SOCKS5 proxy that tunnels TCP connections over the Reticulum Network Stack.
//! Run as either a server (exit node) or client (local SOCKS5 proxy).

use clap::Parser;
use rns_proxy::cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Init logging
    let log_level = if cli.debug { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp_secs()
        .init();

    match cli.command {
        Commands::Server { identity_file } => {
            rns_proxy::server::run_server(identity_file.as_deref()).await;
        }
        Commands::Client {
            destination,
            listen,
        } => {
            rns_proxy::client::run_client(&destination, &listen).await;
        }
    }
}

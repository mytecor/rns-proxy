use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rns-proxy")]
#[command(about = "SOCKS5 proxy over Reticulum Network Stack")]
#[command(version)]
pub struct Cli {
    /// Enable debug logging
    #[arg(long, global = true)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the SOCKS5 proxy server (exit node)
    Server {
        /// Path to the identity file for persistent server address.
        /// Defaults to ~/.reticulum/rns_proxy_identity
        #[arg(long, value_name = "PATH")]
        identity_file: Option<String>,
    },

    /// Run the SOCKS5 proxy client (local proxy)
    Client {
        /// RNS destination hash (hex)
        #[arg(short, long)]
        destination: String,

        /// Local SOCKS5 listen address
        #[arg(short, long, default_value = "127.0.0.1:1080")]
        listen: String,
    },
}

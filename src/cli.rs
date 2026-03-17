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
    Server,

    /// Run the SOCKS5 proxy client (local proxy)
    Client {
        /// RNS address of the server (hex)
        #[arg(long)]
        server: String,

        /// Local SOCKS5 listen port
        #[arg(long, default_value_t = 1080)]
        port: u16,
    },
}

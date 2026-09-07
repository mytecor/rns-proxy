use clap::{Parser, Subcommand};
use prse::try_parse;

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
        #[arg(short, long, value_name = "PATH")]
        identity_file: Option<String>,

        /// prevents a client from connecting to localhost addresses.
        #[clap(long, short, action)]
        localhost_block: bool,

        /// prevents a client from connecting to private addresses.
        #[clap(long, short, action)]
        private_network_block: bool,
        
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
    /// Connects to localhost ports of the server (Port forwarding)
    Connect  {
        /// RNS destination hash (hex)
        #[arg(short, long)]
        destination: String,

        /// connects udp ports, can be specified as just the port number or as SERVER_PORT:CLIENT_PORT. Multiple of this flag can be specified
        #[arg(short, long, value_parser = port_parser)]
        udp_port: Vec<(u16,u16)>,

        /// connectcs tcp ports, can be specified as just the port number or as SERVER_PORT:CLIENT_PORT. Multiple of this flag can be specified
        #[arg(short, long, value_parser = port_parser)]
        tcp_port: Vec<(u16,u16)>,

        /// shorthand for connecting to both udp and tcp, can be specified as just the port number or as SERVER_PORT:CLIENT_PORT. Multiple of this flag can be specified
        #[arg(short, long, value_parser = port_parser)]
        both_port: Vec<(u16,u16)>, // both udp and tcp
    },
    /// Exposes a SOCKS5 proxy that only allows clients to connect to specified localhost ports.
    Forward  {
        #[arg(long, value_name = "PATH")]
        /// Path to the identity file for persistent server address.
        /// Defaults to ~/.reticulum/rns_proxy_identity
        identity_file: Option<String>,

        #[arg(short, long)]
        /// allow connecting to localhost udp port for any client accessing this destination. Multiple of this flag can be specified
        udp_port: Vec<u16>,

        #[arg(short, long)]
        /// allow connecting to localhost tcp port for any client accessing this destination. Multiple of this flag can be specified
        tcp_port: Vec<u16>,

        #[arg(short, long)]
        /// allow connecting to localhost tcp and udp port for any client accessing this destination. Multiple of this flag can be specified 
        both_port: Vec<u16>,    }
}



fn port_parser(s: &str) -> Result<(u16,u16),String> {
    if let Ok((server_port,client_port)) = try_parse!(s,"{}:{}") {
        return Ok((server_port,client_port));
    }
    if let Ok(port) = try_parse!(s,"{}") {
        return Ok((port,port));
    }


    return Err("Invalid port input".into());
}

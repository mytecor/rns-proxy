//! RNS SOCKS5 Proxy Service
//!
//! A SOCKS5 proxy that tunnels TCP connections over the Reticulum Network Stack.
//! Run as either a server (exit node) or client (local SOCKS5 proxy).

use clap::Parser;
use log::LevelFilter;
use rns_proxy::{cli::{Cli, Commands}, client::run_client_forward, filter::{Filter, FilterConfig, FilterResult, PortFilter}, forwarding::{ForwardedPort, PortType}};
use env_logger;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Init logging
    let log_level = if cli.debug { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp_millis()
        .format_source_path(true)
        .filter_module("rns_net", LevelFilter::Warn) // prevent rns_net from spamming as much into the console
        // unless there's actually a problem
        .init();

    match cli.command {
        Commands::Server { identity_file, localhost_block, private_network_block  } => {


            let mut filters = vec![Filter {
                        address_filter: rns_proxy::filter::AddressFilter::All,
                        port_filter: PortFilter{
                            port_filter: rns_proxy::filter::PortFilterType::All,
                            port_type: PortType::TcpUdp,
                        },
                        filter_result: FilterResult::Include,
                    }]; 

            if localhost_block {
                filters.push(
                    Filter {
                        address_filter: rns_proxy::filter::AddressFilter::Localhost,
                        port_filter: PortFilter{
                            port_filter: rns_proxy::filter::PortFilterType::All,
                            port_type: PortType::TcpUdp,
                        },
                        filter_result: FilterResult::Exclude,
                    }
                )
            };
            if private_network_block {
                filters.push(
                    Filter {
                        address_filter: rns_proxy::filter::AddressFilter::Private,
                        port_filter: PortFilter{
                            port_filter: rns_proxy::filter::PortFilterType::All,
                            port_type: PortType::TcpUdp,
                        },
                        filter_result: FilterResult::Exclude,
                    }
                )
            };
            rns_proxy::server::run_server(identity_file.as_deref(),
                FilterConfig {
                    filters: filters,
                }

            ).await;
        }
        Commands::Client {
            destination,
            listen,
        } => {
            rns_proxy::client::run_client(&destination, &listen).await;
        }
        Commands::Connect { destination, tcp_port, udp_port, both_port} => {

            let ports_to_connect = connect_vec_merging_connect(tcp_port, udp_port, both_port);
            run_client_forward(&destination, ports_to_connect).await;
            // rns_proxy::server::run_server(identity_file.as_deref()).await;
        }
        Commands::Forward{identity_file, tcp_port, udp_port, both_port} => {
            // run_client_forward(&destination, vec![ForwardedPort{
            //     server_port: 34197,
            //     client_port: 34197,
            //     r#type: ForwardedPortType::Udp
                
            // }]).await;
            // // rns_proxy::server::run_server(identity_file.as_deref()).await;
            rns_proxy::server::run_server(identity_file.as_deref(), connect_vec_merging_forward(tcp_port,udp_port,both_port)).await;
                
        }
    }
}

fn connect_vec_merging_connect(tcp_port: Vec<(u16,u16)>,udp_port: Vec<(u16,u16)>, both_port: Vec<(u16,u16)>) -> Vec<ForwardedPort> {
    let mut vec = Vec::new();

    // might be a nicer way of simplifying later, idc for now
    for port in tcp_port {
        vec.push(ForwardedPort{ server_port: port.0, client_port: port.1, r#type: PortType::Tcp})
    }
    for port in udp_port {
        vec.push(ForwardedPort{ server_port: port.0, client_port: port.1, r#type: PortType::Udp})
    }
    for port in both_port {
        vec.push(ForwardedPort{ server_port: port.0, client_port: port.1, r#type: PortType::TcpUdp})
    }


    vec
}

fn connect_vec_merging_forward(tcp_port: Vec<u16>,udp_port: Vec<u16>, both_port: Vec<u16>) -> FilterConfig {
    let mut vec = Vec::new();

    // oh my boilerplate
    for port in tcp_port {
       vec.push(Filter {
            address_filter: rns_proxy::filter::AddressFilter::Localhost,
            port_filter: PortFilter{
                port_filter: rns_proxy::filter::PortFilterType::Single(port),
                port_type: PortType::Tcp,
            },
            filter_result: FilterResult::Include,
        }); 
    }
    for port in udp_port {
       vec.push(Filter {
            address_filter: rns_proxy::filter::AddressFilter::Localhost,
            port_filter: PortFilter{
                port_filter: rns_proxy::filter::PortFilterType::Single(port),
                port_type: PortType::Udp,
            },
            filter_result: FilterResult::Include,
        }); 
    }
    for port in both_port {
       vec.push(Filter {
            address_filter: rns_proxy::filter::AddressFilter::Localhost,
            port_filter: PortFilter{
                port_filter: rns_proxy::filter::PortFilterType::Single(port),
                port_type: PortType::TcpUdp,
            },
            filter_result: FilterResult::Include,
        }); 
    };
    return FilterConfig { filters: vec }
}

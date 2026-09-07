use std::net::{IpAddr::{self, V4, V6},  SocketAddr};

use fast_socks5::util::target_addr::TargetAddr;
use log::warn;

use crate::{filter::{AddressFilter::{Address, Localhost, Private}, PortFilterType::Single}, forwarding::PortType,  };

///! Socket Filter
///! 
///! This allows the server side client to restrict what ips are allowed. as well as what ports 
///! 
///! This allows for generals socksv5 proxying while for example disabling accessing the localhost or computer's private network or disabling all UDP.
///! but can also be used for more restrictive purposes like only allowing clients to connect to a certain localhost.
///!
///! # Filter list  
///! the filter list consists of a list of filters which can either be include or exclude
///! with the top having the least prioty and more granular settings and the bottom having the most priorty. 
///! 
///! by defeault an addressj
///! 
///! # examples
///! ## example 1, only allow dns requests
///!  All:53:udp Include
///! ## example 2, allow connections only to a local IRC chat 
///!  All:194 Include
///! ## example 3, generic sockets server that blocks udp, private and localhost except for a localhost web server
///!  All Include
///!  Private Exclude
///!  Localhost Exclude
///!  Localhost:80:tcp
///!


pub trait FilterSocket {
    fn filter(&self, addr: &SocketAddr) -> bool;
}

#[derive(Clone)]
pub enum FilterResult {
    Exclude,
    Include,
} // filters can also do neither if they are not applicable.
 

#[derive(Clone)]
pub enum AddressFilter {
    All,
    Address(IpAddr),
    Private,
    Localhost,
}
impl FilterSocket for AddressFilter {
    fn filter(&self, addr: &SocketAddr) -> bool {
        match *self {
            AddressFilter::All => true,
            Address(filter) => addr.ip() == filter,
            Private => match addr.ip() {
                V4(addr) => addr.is_private(),
                V6(addr) => addr.is_unique_local(),
            },
            Localhost => match addr.ip() {
                V4(addr) => addr.is_loopback(),
                V6(addr) => addr.is_loopback(),
            }
        }
    }
}

#[derive(Clone)]
pub enum PortFilterType {
    Single(u16),
    All,
}

#[derive(Clone)]
pub struct PortFilter {
    pub port_filter: PortFilterType,
    pub port_type: PortType,
}

impl FilterSocket for PortFilter {
    fn filter(&self, addr: &SocketAddr) -> bool {
        match self.port_filter {
            Single(checking_port) => {
                addr.port() == checking_port
            },
            PortFilterType::All => true,
        }
    }
}

#[derive(Clone)]
pub struct Filter {
    pub address_filter: AddressFilter,
    pub port_filter: PortFilter,
    pub filter_result: FilterResult, 
}

#[derive(Clone)]
pub struct FilterConfig {
    pub filters: Vec<Filter>, // last filter has the most priority.
}

/// Note that this functionality could potentially be used to expose the real ip address of the server
/// by making the reticulum server do a bogus dns request and using that a marker to find where it came from.
/// Unlikey but possible and filtering by everything wouldn't prevent this. as we perform filtering
/// after having having resolved the real ip. Adding a "deny domainname" to FilterConfig may make sense
/// to reduce the possible attack surface
/// 
/// Something to keep in mind.


pub async fn target_to_socket(addr: TargetAddr) -> Option<SocketAddr> {
    match addr.resolve_dns().await {
        Ok(socket_packed) => { // idk why this function signature is like this.
            if let TargetAddr::Ip(socket) = socket_packed {
                Some(socket)
            } else {
                // this can never happen
                warn!("this did happen");
                return None; 
            }
        }
        Err(_) => {
            return None;
        }
        
    }
}


pub async fn allowed_ip(socket_addr: SocketAddr, filter_config: &FilterConfig, port_type: PortType) -> bool {
    let mut include = false; // by default we don't allow anything.

    for filter in &filter_config.filters {
        let type_matches = matches!(
            (&filter.port_filter.port_type, &port_type),
            (&PortType::Tcp, &PortType::Tcp)
                | (&PortType::Udp, &PortType::Udp)
                | (&PortType::TcpUdp, _)
        );
        if type_matches && filter.address_filter.filter(&socket_addr) && filter.port_filter.filter(&socket_addr) {
            match filter.filter_result {
                FilterResult::Exclude => {include = false}
                FilterResult::Include => {include = true}
            }
        }
    }

    include
}

/// note that we are converting from TargetAddr to SocketAddr as well as filtering
/// the difference being that TargetAddr can also be a domain name and port number
/// instead of an ip address and port number. Most functions require ip rather than
/// domain name and resolving dns and then using it to connect makes everything a
/// this means that if the user has put a hostname into the socks proxy we are actually
/// getting the real ip and then passing it back. This is prevent some sneaky stuff from happening.
///
/// also we don't currently discriminate against tcp or udp. that's a todo for later
pub async fn filter_and_convert(addr: TargetAddr, filter_config: Option<&FilterConfig>, port_type: PortType) -> Option<SocketAddr> {
    let socket_addr = target_to_socket(addr).await?;
    if let Some(filter_config) = filter_config {
        if allowed_ip(socket_addr, filter_config, port_type).await {
           return Some(socket_addr) 
        } else {
            return None
        }

    } else {
        Some(socket_addr)
    }
    
}

#[cfg(test)]
mod tests {
    use super::{allowed_ip, AddressFilter, Filter, FilterConfig, FilterResult, PortFilter, PortFilterType};
    use crate::forwarding::PortType;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn port_type_is_enforced() {
        let config = FilterConfig {
            filters: vec![Filter {
                address_filter: AddressFilter::Address(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port_filter: PortFilter {
                    port_filter: PortFilterType::Single(25565),
                    port_type: PortType::Tcp,
                },
                filter_result: FilterResult::Include,
            }],
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 25565));
        assert!(allowed_ip(addr, &config, PortType::Tcp).await);
        assert!(!allowed_ip(addr, &config, PortType::Udp).await);
    }
}

//! Bidirectional TCP?UDP ↔ RNS relay — used by both client and server sessions.
//!
//! Dev Note 1, idk where to put this but the buffer could be smaller than 

use std::net::Ipv4Addr;
use std::sync::Arc;

 
use fast_socks5::{new_udp_header, parse_udp_request};
use fast_socks5::util::target_addr::ToTargetAddr;
use log::{debug, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};
use udp_stream::UdpStream;

use crate::filter::{FilterConfig, allowed_ip, filter_and_convert};
use crate::forwarding::PortType;
use crate::frame::{Frame, FrameType};
use crate::mux::MuxHandle;

/// Relay data bidirectionally between a TCP stream and an RNS mux session.
///
/// Sends a `Close` frame and drops the session when either direction finishes.
pub async fn relay_bidirectional_tcp(
    sid: u32,
    stream: tokio::net::TcpStream,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let mux_fwd = mux.clone();


    debug!("[{}] relay bidirectional tcp", sid);

    // TCP -> RNS
    let mut tcp_to_rns = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    mux_fwd.send(FrameType::Data, sid, buf[..n].to_vec()).await;
                }
                Err(e) => {
                    debug!("[{}] TCP read error: {}", sid, e);
                    break;

                }
            }
        }
    });

    // RNS -> TCP
    let mut rns_to_tcp = tokio::spawn(async move {
        while let Some(frame) = session_rx.recv().await {
            match frame.frame_type {
                FrameType::Data => {
                    // info!("{:?}", &frame.payload);
                    if let Err(e) = tcp_write.write_all(&frame.payload).await {
                        warn!("[{}] TCP write error: {}", sid, e);
                        break;
                    }
                }
                FrameType::Close => break,
                FrameType::ConnectErr => {warn!("connection errored, should probably be handled by different code"); break},
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut tcp_to_rns => { rns_to_tcp.abort(); },
        _ = &mut rns_to_tcp => { tcp_to_rns.abort(); },
    }

    debug!("[{}] drop connection", sid);

    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid).await;
}



/// udp client side must be split off to due a lot of things

pub async fn relay_bidirectional_udp_client_side(
    sid: u32,
    socket: tokio::net::UdpSocket,
    tcp_stream: tokio::net::TcpStream,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    
    let socket = Arc::new(socket);
    let socket1 = socket.clone();

    let mux_fwd = mux.clone();

    let client_local_port_mutex = Mutex::new(None);
    let client_local_port_1 = Arc::new(client_local_port_mutex);
    let client_local_port_2 = client_local_port_1.clone();
    // the socksv5 protocol never sends the port number that the
    // udp relay server should be expecting from the client, and the port number for the udp
    // connection may be different that the tcp port. So the only way to get the port that the client
    // is expecting is to wait until the client has sent data through udp and record that data
    // this does mean that if the client is only ever receiving data from udp, it will never get
    // the right port number because the relay will never know which port to send it through.
    // I reckon that situation is very rare and that most socksv5 clients will account for that
    // but it's possible this breaks here.
    //
    // I might just be being stupid but I can't find a better way of figuring this out. and the spec
    // is pretty vague so https://www.rfc-editor.org/info/rfc1928/
    //
    // you could probably do better than using a mutex cause it's only updated in one thread
    // and read in another, but I odn't know enough fancy rust stuff to actually do that.


    // UDP -> RNS
    let udp_to_rns = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
             let stuff = socket.recv_from(&mut buf).await;
             
             match stuff {
                Ok((0,_)) => { break},
                Ok((n,addr)) => {
                    
                    
                    let mut a =  client_local_port_1.lock().await; *a = Some(addr.port());

                    mux_fwd.send(FrameType::Data, sid, buf[..n].to_vec()).await;
                }
                Err(e) => {
                    debug!("[{}] UDP read error: {}", sid, e);
                    break;
                }
            }
        }
    });

    // RNS -> UDP
    let rns_to_udp = tokio::spawn(async move {
        loop {
            if let Some(frame) = session_rx.recv().await {
                
                
                match frame.frame_type {
                    FrameType::Data => {
                       let a =  client_local_port_2.lock().await;
                       let value = *a;

                       if let Some(port) = value {
                            
                            if let Err(e) = socket1.send_to(&frame.payload, (Ipv4Addr::LOCALHOST,port)).await {
                                warn!("[{}] UDP write error: {}", sid, e);
                                break;
                            } else {
                                                            };
                           
                       } else {
                           warn!("UDP received but client side does not know of a port");
                           // break // shouldn't break because this might not be the client's fault
                           // maybe some random bot send a udp request to that port before the client
                           // could do anything, so we just leave it open.
                       }  
                    }
                    FrameType::Close => { break},
                    _ => {}
                }
            } else {
                
                break;
                
             };
            
        }
        
    });

    let (mut tcp_read, mut _tcp_write) = tcp_stream.into_split();
    let break_connection_tcp_check = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => {
                    debug!("tcp connectioned associated with udp died {:?}", sid);
                    break;
                },
                Ok(n) => {
                    warn!("client still sending tcp through udp port {:?} {:?}", sid, &buf[0..n])
                }
                Err(e) => {
                    debug!("[{}] TCP read error: {}", sid, e);
                    break;
                }
            }
        }
    });
    tokio::select! {
        _ = udp_to_rns => {},
        _ = rns_to_udp => {},
        _ = break_connection_tcp_check => {}
    }

    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid).await;
}
pub async fn relay_bidirectional_udp_server_side(
    sid: u32,
    socket: tokio::net::UdpSocket,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
    filter_config: FilterConfig,
) {
    let socket = Arc::new(socket);
    let socket1 = socket.clone();

    let config1 = Arc::new(filter_config); // someone please figure out a better way of doing this
    let config2 = config1.clone();

    let mux_fwd = mux.clone();
    

    // UDP -> RNS
    let udp_to_rns = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
             let stuff = socket.recv_from(&mut buf).await;
             
             match stuff {
                Ok((0,_)) => { break},
                Ok((n,addr)) => {
                    if allowed_ip(addr, &config1, PortType::Udp).await {
                        let mut packet = new_udp_header(addr).expect("cannot wrap udp packet");
                        packet.extend_from_slice(&buf[..n]);
                        
                        mux_fwd.send(FrameType::Data, sid, packet.to_vec()).await;
                    } else {
                        warn!("packet came from illegal server location")
                    }
                }
                Err(e) => {
                    debug!("[{}] UDP read error: {}", sid, e);
                    break;
                }
            }
        }
    });

    // RNS -> UDP
    let rns_to_udp = tokio::spawn(async move {
        loop {
            if let Some(frame) = session_rx.recv().await {
                
                
                match frame.frame_type {
                    FrameType::Data => {
                        match parse_udp_request(&*frame.payload).await {
                            Ok((_frag,addr,data)) => {
                                

                                if let Some(socket) = filter_and_convert(addr.clone(), Some(&config2), PortType::Udp).await {
                                    
                                    if let Err(e) = socket1.send_to(data,socket).await {
                                        warn!("[{}] UDP write error: {}", sid, e);
                                        break;
                                    } else {
                                                                            }
                                } else {
                                    warn!("[{}] client attempted to send to illegal location {}",sid, addr)
                                }
                        
                            }
                            Err(e) => {
                                debug!("[{}] UDP read error: {}", sid, e);
                                break

                            }
                    
                        };

                    }
                    FrameType::Close => { break},
                    _ => {}
                }
            } else {
                
                break;
                
             };
            
        }
        
    });


    tokio::select! {
        _ = udp_to_rns => {},
        _ = rns_to_udp => {},
    }

    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid).await;
}

/// udp must still have an associated tcp connection to detect when the connection is over. 
/// this does not apply to the server (as in rns server) as it detects that the frame is being closed
/// udp doesn't have a connetcion so the server cannot detect that the remote server the client is connecting
/// to is offline per say.
/// 
/// basically, the client can stop the udp connection either by the reticulum link breaking
/// OR the process using the udp connection stops
/// while the server only stops in the first scenario because the server cannot know if the remote
/// server not responding is part of the protocol.



pub async fn relay_forwarded_tcp(
    sid: u32,
    stream: tokio::net::TcpStream, // stream between local forwarded port and the port
    // of whatever application is connecting to it.
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>)
{
    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let mux_fwd = mux.clone();

    debug!("relay forwarded tcp");

    // TCP -> RNS
    let mut tcp_to_rns = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    
                    mux_fwd.send(FrameType::Data, sid, buf[..n].to_vec()).await;
                }
                Err(e) => {
                    debug!("[{}] TCP read error: {}", sid, e);
                    break;

                }
            }
        }
    });

    // RNS -> TCP
    let mut rns_to_tcp = tokio::spawn(async move {
        while let Some(frame) = session_rx.recv().await {
            match frame.frame_type {
                FrameType::Data => {
                    
                    if let Err(e) = tcp_write.write_all(&frame.payload).await {
                        warn!("[{}] TCP write error: {}", sid, e);
                        break;
                    }
                }
                FrameType::Close => break,
                FrameType::ConnectErr => {warn!("connection errored, should probably be handled by different code"); break},
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut tcp_to_rns => { rns_to_tcp.abort(); },
        _ = &mut rns_to_tcp => { tcp_to_rns.abort(); },
    }

    debug!("[{}] tcp relay ended", sid);
    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid).await;
}

pub async fn relay_forwarded_udp(
    sid: u32,
    stream: UdpStream, // stream between local forwarded port and the port
    // of whatever application is connecting to it.
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
    server_port: u16)
{
    let mux_fwd = mux.clone();

    let localhost_server_addr = (Ipv4Addr::LOCALHOST, server_port).to_target_addr().unwrap();

    let (mut udp_read,mut udp_write) = tokio::io::split(stream);

    // UDP -> RNS
    let tcp_to_rns = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match udp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    
                    let mut packet = new_udp_header(localhost_server_addr.clone() ).expect("cannot wrap udp packet");
                    packet.extend_from_slice(&buf[..n]);
                    
                    mux_fwd.send(FrameType::Data, sid, packet.to_vec()).await;
                }
                Err(e) => {
                    debug!("[{}] TCP read error: {}", sid, e);
                    break;

                }
            }
        }
    });

    // RNS -> UDP
    let rns_to_tcp = tokio::spawn(async move {
        while let Some(frame) = session_rx.recv().await {
            match frame.frame_type {
                FrameType::Data => {
                    
                    match parse_udp_request(&*frame.payload).await {
                        Ok((_frag,_addr,data)) => {
                            // let target  = addr.into_string_and_port();
                
                            if let Err(e) = udp_write.write_all(data).await {
                                warn!("[{}] UDP write error: {}", sid, e);
                                break;
                            } else {
                                                            }
                        }
                        Err(e) => {
                            debug!("[{}] UDP read error: {}", sid, e);
                            break

                        }
            
                    };
                }
                FrameType::Close => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = tcp_to_rns => {},
        _ = rns_to_tcp => {},
    }

    debug!("[{}] udp relay ended", sid);
    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid).await;
}


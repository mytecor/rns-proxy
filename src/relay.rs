//! Bidirectional TCP and UDP ↔ RNS relay — used by both client and server sessions.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fast_socks5::util::target_addr::TargetAddr;
use log::{debug, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::frame::{Frame, FrameType};
use crate::mux::MuxHandle;

/// Relay data bidirectionally between a TCP stream and an RNS mux session.
///
/// Sends a `Close` frame and drops the session when either direction finishes.
pub async fn relay_bidirectional(
    sid: u32,
    stream: tokio::net::TcpStream,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let mux_fwd = mux.clone();

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
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut tcp_to_rns => { rns_to_tcp.abort(); }
        _ = &mut rns_to_tcp => { tcp_to_rns.abort(); }
    }

    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid);
}

/// Relay UDP datagrams between a local SOCKS5 UDP socket and an RNS mux session.
///
/// In accordance with RFC 1928, a UDP association terminates when the TCP connection
/// that initiated the UDP ASSOCIATE request terminates.
pub async fn relay_udp_client(
    sid: u32,
    socket: tokio::net::UdpSocket,
    tcp_stream: tokio::net::TcpStream,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    let expected_ip = match tcp_stream.peer_addr() {
        Ok(peer) => peer.ip(),
        Err(_) => {
            mux.send(FrameType::Close, sid, Vec::new()).await;
            mux.drop_session(sid);
            return;
        }
    };
    let socket = Arc::new(socket);
    let socket_recv = Arc::clone(&socket);
    let socket_send = Arc::clone(&socket);
    let mux_fwd = mux.clone();

    // The client application's local UDP socket address from which it sends datagrams.
    let client_addr: Arc<std::sync::Mutex<Option<SocketAddr>>> =
        Arc::new(std::sync::Mutex::new(None));
    let client_addr_recv = Arc::clone(&client_addr);
    let client_addr_send = Arc::clone(&client_addr);

    // 1. Client App UDP -> RNS
    let mut udp_to_rns = tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((n, addr)) => {
                    if addr.ip() != expected_ip {
                        continue;
                    }
                    // Validate before learning the endpoint; malformed packets cannot claim it.
                    if !matches!(
                        fast_socks5::parse_udp_request(&buf[..n]).await,
                        Ok((0, _, _))
                    ) {
                        continue;
                    }
                    {
                        let mut endpoint = client_addr_recv.lock().unwrap();
                        if endpoint.is_some_and(|known| known != addr) {
                            continue;
                        }
                        *endpoint = Some(addr);
                    }
                    mux_fwd.send(FrameType::Data, sid, buf[..n].to_vec()).await;
                }
                Err(e) => {
                    debug!("[{}] Client UDP read error: {}", sid, e);
                    break;
                }
            }
        }
    });

    // 2. RNS -> Client App UDP
    let mut rns_to_udp = tokio::spawn(async move {
        while let Some(frame) = session_rx.recv().await {
            match frame.frame_type {
                FrameType::Data => {
                    let maybe_addr = *client_addr_send.lock().unwrap();
                    if let Some(target_addr) = maybe_addr {
                        if let Err(e) = socket_send.send_to(&frame.payload, target_addr).await {
                            warn!("[{}] Client UDP send_to {} error: {}", sid, target_addr, e);
                            break;
                        }
                    } else {
                        warn!(
                            "[{}] Dropping incoming UDP packet: client address unknown yet",
                            sid
                        );
                    }
                }
                FrameType::Close => break,
                _ => {}
            }
        }
    });

    // 3. Monitor TCP control connection (RFC 1928 Section 6)
    let (mut tcp_read, _tcp_write) = tcp_stream.into_split();
    let mut tcp_monitor = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => {
                    debug!("[{}] SOCKS5 TCP control connection closed by client", sid);
                    break;
                }
                Ok(_) => {
                    // Ignore extra data on TCP control channel
                }
                Err(e) => {
                    debug!("[{}] SOCKS5 TCP control error: {}", sid, e);
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = &mut udp_to_rns => {
            rns_to_udp.abort();
            tcp_monitor.abort();
        }
        _ = &mut rns_to_udp => {
            udp_to_rns.abort();
            tcp_monitor.abort();
        }
        _ = &mut tcp_monitor => {
            udp_to_rns.abort();
            rns_to_udp.abort();
        }
    }

    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid);
}

/// Relay UDP datagrams between remote hosts and an RNS mux session on the exit server.
pub async fn relay_udp_server(
    sid: u32,
    socket: tokio::net::UdpSocket,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    let is_ipv6 = socket.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
    let last_activity = Arc::new(std::sync::Mutex::new(tokio::time::Instant::now()));
    let incoming_activity = Arc::clone(&last_activity);
    let socket = Arc::new(socket);
    let socket_send = Arc::clone(&socket);
    let socket_recv = Arc::clone(&socket);
    let mux_fwd = mux.clone();

    // 1. RNS -> Target Remote Hosts
    let mut rns_to_udp = tokio::spawn(async move {
        const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
        loop {
            let deadline = *last_activity.lock().unwrap() + IDLE_TIMEOUT;
            let recv_res = tokio::time::timeout_at(deadline, session_rx.recv()).await;
            let frame = match recv_res {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => {
                    if last_activity.lock().unwrap().elapsed() < IDLE_TIMEOUT {
                        continue;
                    }
                    debug!("[{}] Server UDP session idle timeout (300s)", sid);
                    break;
                }
            };

            match frame.frame_type {
                FrameType::Data => {
                    *last_activity.lock().unwrap() = tokio::time::Instant::now();
                    match fast_socks5::parse_udp_request(&frame.payload).await {
                        Ok((frag, target_addr, data)) => {
                            if frag != 0 {
                                warn!(
                                    "[{}] Dropping fragmented SOCKS5 UDP datagram (FRAG={})",
                                    sid, frag
                                );
                                continue;
                            }
                            match target_addr.resolve_dns().await {
                                Ok(TargetAddr::Ip(dest_addr)) => {
                                    let dest_addr = match dest_addr {
                                        SocketAddr::V4(a) if is_ipv6 => SocketAddr::new(
                                            a.ip().to_ipv6_mapped().into(),
                                            a.port(),
                                        ),
                                        a => a,
                                    };
                                    if let Err(e) = socket_send.send_to(data, dest_addr).await {
                                        warn!("[{}] UDP send_to {} error: {}", sid, dest_addr, e);
                                    }
                                }
                                Ok(TargetAddr::Domain(..)) => unreachable!(),
                                Err(e) => {
                                    warn!("[{}] Failed to resolve target address: {:?}", sid, e);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("[{}] Invalid SOCKS5 UDP request: {:?}", sid, e);
                        }
                    }
                }
                FrameType::Close => break,
                _ => {}
            }
        }
    });

    // 2. Target Remote Hosts -> RNS
    let mut udp_to_rns = tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((n, remote_addr)) => {
                    *incoming_activity.lock().unwrap() = tokio::time::Instant::now();
                    let remote_addr = match remote_addr {
                        SocketAddr::V6(a) => a
                            .ip()
                            .to_ipv4_mapped()
                            .map(|ip| SocketAddr::new(ip.into(), a.port()))
                            .unwrap_or(remote_addr),
                        a => a,
                    };
                    match fast_socks5::new_udp_header(remote_addr) {
                        Ok(mut packet) => {
                            if packet.len() + n > u16::MAX as usize {
                                warn!("[{}] UDP response exceeds frame payload limit", sid);
                                continue;
                            }
                            packet.extend_from_slice(&buf[..n]);
                            mux_fwd.send(FrameType::Data, sid, packet).await;
                        }
                        Err(e) => {
                            warn!(
                                "[{}] Failed to construct UDP header for {}: {:?}",
                                sid, remote_addr, e
                            );
                        }
                    }
                }
                Err(e) => {
                    debug!("[{}] Remote UDP read error: {}", sid, e);
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = &mut rns_to_udp => {
            udp_to_rns.abort();
        }
        _ = &mut udp_to_rns => {
            rns_to_udp.abort();
        }
    }

    mux.send(FrameType::Close, sid, Vec::new()).await;
    mux.drop_session(sid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_relay_udp_server_and_client_roundtrip() {
        // 1. Start a mock echo UDP server (representing a remote UDP service)
        let echo_server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo_server.local_addr().unwrap();

        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok((n, sender)) = echo_server.recv_from(&mut buf).await {
                let _ = echo_server.send_to(&buf[..n], sender).await;
            }
        });

        // 2. Set up server-side relay with mock mux
        let (server_mux, mut server_mux_rx) = MuxHandle::new_test();
        let server_session_rx = server_mux.register_session(1);
        let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        tokio::spawn(relay_udp_server(
            1,
            server_socket,
            server_mux.clone(),
            server_session_rx,
        ));

        // 3. Set up client-side relay with mock mux and connected TCP control connection
        let (client_mux, mut client_mux_rx) = MuxHandle::new_test();
        let client_session_rx = client_mux.register_session(1);
        let client_relay_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_relay_addr = client_relay_socket.local_addr().unwrap();

        let tcp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = tcp_listener.local_addr().unwrap();
        let client_tcp = tokio::net::TcpStream::connect(listener_addr).await.unwrap();
        let (server_tcp, _) = tcp_listener.accept().await.unwrap();

        tokio::spawn(relay_udp_client(
            1,
            client_relay_socket,
            server_tcp,
            client_mux.clone(),
            client_session_rx,
        ));

        // 4. Bridge server_mux and client_mux (simulating RNS link)
        let c_mux = client_mux.clone();
        tokio::spawn(async move {
            while let Some(frame) = server_mux_rx.recv().await {
                c_mux.dispatch(frame);
            }
        });
        let s_mux = server_mux.clone();
        tokio::spawn(async move {
            while let Some(frame) = client_mux_rx.recv().await {
                s_mux.dispatch(frame);
            }
        });

        // 5. Client application sends SOCKS5-wrapped UDP datagram to client_relay_addr
        let client_app_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let mut req_packet = fast_socks5::new_udp_header(echo_addr).unwrap();
        req_packet.extend_from_slice(b"hello-reticulum-udp");

        client_app_socket
            .send_to(&req_packet, client_relay_addr)
            .await
            .unwrap();

        // 6. Receive response from client_relay_addr
        let mut resp_buf = [0u8; 1024];
        let (n, resp_src) = tokio::time::timeout(
            Duration::from_secs(3),
            client_app_socket.recv_from(&mut resp_buf),
        )
        .await
        .expect("timeout waiting for UDP echo response")
        .expect("recv_from failed");

        assert_eq!(resp_src, client_relay_addr);
        let (frag, src_addr, data) = fast_socks5::parse_udp_request(&resp_buf[..n])
            .await
            .expect("should be valid SOCKS5 UDP datagram");

        assert_eq!(frag, 0);
        let (src_host, src_port) = src_addr.into_string_and_port();
        assert_eq!(src_host, echo_addr.ip().to_string());
        assert_eq!(src_port, echo_addr.port());
        assert_eq!(data, b"hello-reticulum-udp");

        // 7. Close client TCP control stream -> should terminate UDP relay
        drop(client_tcp);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    async fn settle() {
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn udp_client_rejects_invalid_packets_and_endpoint_changes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let honest = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rogue = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (mux, mut frames) = MuxHandle::new_test();
        let rx = mux.register_session(1);
        let relay = tokio::spawn(relay_udp_client(1, socket, tcp, mux.clone(), rx));
        rogue.send_to(&[], addr).await.unwrap();
        rogue.send_to(&[42], addr).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), frames.recv())
                .await
                .is_err()
        );
        let mut packet = fast_socks5::new_udp_header(addr).unwrap();
        packet.extend_from_slice(b"request");
        // Where the OS provides additional loopback addresses, also check a foreign IP.
        if let Ok(foreign) = tokio::net::UdpSocket::bind("127.0.0.2:0").await {
            foreign.send_to(&packet, addr).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), frames.recv())
                    .await
                    .is_err()
            );
        }
        honest.send_to(&packet, addr).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), frames.recv())
                .await
                .unwrap()
                .unwrap()
                .payload,
            packet
        );
        rogue.send_to(&packet, addr).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), frames.recv())
                .await
                .is_err()
        );
        mux.dispatch(Frame::new(FrameType::Data, 1, b"response".to_vec()));
        let mut buf = [0; 64];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), honest.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"response");
        drop(control);
        tokio::time::timeout(Duration::from_secs(2), relay)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn udp_exit_supports_both_families_and_empty_responses() {
        let socket = crate::server::bind_udp_exit_socket().await.unwrap();
        let (mux, mut frames) = MuxHandle::new_test();
        let rx = mux.register_session(1);
        let relay = tokio::spawn(relay_udp_server(1, socket, mux.clone(), rx));
        for bind in ["127.0.0.1:0", "[::1]:0"] {
            let remote = tokio::net::UdpSocket::bind(bind).await.unwrap();
            let addr = remote.local_addr().unwrap();
            let mut packet = fast_socks5::new_udp_header(addr).unwrap();
            packet.extend_from_slice(b"request");
            mux.dispatch(Frame::new(FrameType::Data, 1, packet));
            let mut buf = [0; 64];
            let (_, exit) =
                tokio::time::timeout(Duration::from_secs(2), remote.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
            for data in [&b""[..], &b"still alive"[..]] {
                remote.send_to(data, exit).await.unwrap();
                let frame = tokio::time::timeout(Duration::from_secs(2), frames.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(frame.frame_type, FrameType::Data);
                let (_, source, payload) = fast_socks5::parse_udp_request(&frame.payload)
                    .await
                    .unwrap();
                assert_eq!(source, TargetAddr::Ip(addr));
                assert_eq!(payload, data);
            }
        }
        mux.dispatch(Frame::new(FrameType::Close, 1, vec![]));
        tokio::time::timeout(Duration::from_secs(2), relay)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn udp_downstream_activity_resets_idle_timeout() {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let remote = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (mux, mut frames) = MuxHandle::new_test();
        let rx = mux.register_session(1);
        let relay = tokio::spawn(relay_udp_server(1, socket, mux, rx));
        remote.send_to(b"initial", addr).await.unwrap();
        assert_eq!(frames.recv().await.unwrap().frame_type, FrameType::Data);
        tokio::time::pause();
        // Keep the runtime ready so virtual time advances only explicitly during socket I/O.
        let ready = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        for _ in 0..31 {
            tokio::time::advance(Duration::from_secs(10)).await;
            remote.send_to(b"stream", addr).await.unwrap();
            settle().await;
            assert_eq!(frames.try_recv().unwrap().frame_type, FrameType::Data);
        }
        tokio::time::advance(Duration::from_secs(301)).await;
        settle().await;
        assert_eq!(frames.try_recv().unwrap().frame_type, FrameType::Close);
        relay.await.unwrap();
        ready.abort();
    }
}

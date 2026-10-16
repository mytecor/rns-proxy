use std::time::Duration;

use fast_socks5::client::Socks5Datagram;
use fast_socks5::util::target_addr::TargetAddr;
use rns_proxy::frame::{decode_connect_payload, FrameType};
use rns_proxy::mux::MuxHandle;
use rns_proxy::relay::{relay_udp_client, relay_udp_server};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

#[tokio::test]
async fn test_socks5_udp_associate_end_to_end() {
    // 1. Mock Remote UDP echo server
    let echo_server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo_server.local_addr().unwrap();

    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((n, sender)) = echo_server.recv_from(&mut buf).await {
            let _ = echo_server.send_to(&buf[..n], sender).await;
        }
    });

    // 2. Setup mock server MuxHandle
    let (server_mux, mut server_mux_rx) = MuxHandle::new_test();
    let (client_mux, mut client_mux_rx) = MuxHandle::new_test();

    // 3. Setup client SOCKS5 TCP listener
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks5_addr = listener.local_addr().unwrap();

    // Accept loop for client SOCKS5 proxy
    let c_mux_clone = client_mux.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let sid = c_mux_clone.next_session_id();
            let session_rx = c_mux_clone.register_session(sid);
            let m = c_mux_clone.clone();
            tokio::spawn(async move {
                // SOCKS5 handshake via fast-socks5
                let proto = fast_socks5::server::Socks5ServerProtocol::accept_no_auth(stream)
                    .await
                    .unwrap();
                let (proto, cmd, target_addr) = proto.read_command().await.unwrap();
                assert_eq!(cmd, fast_socks5::Socks5Command::UDPAssociate);

                let (host, port) = target_addr.into_string_and_port();
                let udp_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let relay_addr = udp_socket.local_addr().unwrap();

                let connect_payload = rns_proxy::encode_connect_payload(&host, port, true);
                m.send(FrameType::Connect, sid, connect_payload).await;

                // Wait for ConnectOk
                let mut s_rx = session_rx;
                while let Some(frame) = s_rx.recv().await {
                    if frame.frame_type == FrameType::ConnectOk {
                        break;
                    }
                }

                let stream = proto.reply_success(relay_addr).await.unwrap();
                relay_udp_client(sid, udp_socket, stream, m, s_rx).await;
            });
        }
    });

    // 4. Bridge client and server muxes (simulating Reticulum link)
    let s_mux_for_bridge = server_mux.clone();
    tokio::spawn(async move {
        while let Some(frame) = client_mux_rx.recv().await {
            match frame.frame_type {
                FrameType::Connect => {
                    let sid = frame.session_id;
                    let (_host, _port, is_udp) =
                        decode_connect_payload(&frame.payload).expect("valid connect payload");
                    assert!(is_udp);

                    let session_rx = s_mux_for_bridge.register_session(sid);
                    let s_socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();
                    s_mux_for_bridge
                        .send(FrameType::ConnectOk, sid, Vec::new())
                        .await;

                    let m = s_mux_for_bridge.clone();
                    tokio::spawn(async move {
                        relay_udp_server(sid, s_socket, m, session_rx).await;
                    });
                }
                _ => {
                    s_mux_for_bridge.dispatch(frame);
                }
            }
        }
    });

    let c_mux_for_bridge = client_mux.clone();
    tokio::spawn(async move {
        while let Some(frame) = server_mux_rx.recv().await {
            c_mux_for_bridge.dispatch(frame);
        }
    });

    // 5. Test using standard SOCKS5 client Socks5Datagram
    let backing_tcp = TcpStream::connect(socks5_addr).await.unwrap();
    let datagram = Socks5Datagram::bind(backing_tcp, "127.0.0.1:0")
        .await
        .expect("Socks5Datagram::bind failed");

    let message = b"testing Reticulum SOCKS5 UDP tunneling!";
    datagram
        .send_to(message, echo_addr)
        .await
        .expect("send_to failed");

    let mut buf = [0u8; 1024];
    let (n, from_addr) = tokio::time::timeout(Duration::from_secs(3), datagram.recv_from(&mut buf))
        .await
        .expect("timeout waiting for response")
        .expect("recv_from error");

    assert_eq!(&buf[..n], message);
    assert_eq!(from_addr, TargetAddr::Ip(echo_addr));
}

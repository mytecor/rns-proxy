//! RNS SOCKS5 client -- local SOCKS5 server that tunnels through RNS.
//!
//! Equivalent of `rns_socks_client.py`.
//!
//! The client:
//! 1. Starts an RNS node, waits for a path to the server destination.
//! 2. Creates an RNS link to the server.
//! 3. Listens on a local TCP port for SOCKS5 connections.
//! 4. For each SOCKS5 CONNECT, sends a CONNECT frame through the mux and
//!    relays data bidirectionally.
//! 5. Automatically reconnects when the link is lost.
//!
//! SOCKS5 protocol handling is delegated to `fast-socks5`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fast_socks5::server::Socks5ServerProtocol;
use fast_socks5::{ReplyError, Socks5Command};
use log::{debug, error, info, warn};
use rns_net::{LinkId, RnsNode};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};

use crate::mux::MuxHandle;
use crate::{
    create_node, encode_connect_payload, ensure_path, recall_sig_pub, relay_bidirectional, Frame,
    FrameType, ProxyEvent,
};

/// Run the SOCKS5 client.
pub async fn run_client(server_hex: &str, port: u16) {
    let server_dest_hash: [u8; 16] = match hex::decode(server_hex) {
        Ok(v) if v.len() == 16 => {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(&v);
            arr
        }
        _ => {
            error!("Invalid server address: must be 32 hex chars (16 bytes)");
            return;
        }
    };

    let (node, mut rx) = match create_node() {
        Ok(v) => v,
        Err(e) => {
            error!("{}", e);
            return;
        }
    };

    let mux = MuxHandle::new(Arc::clone(&node));

    // Initial path + link establishment
    info!("Looking for route to {}...", server_hex);
    let sig_pub_bytes = wait_for_path(&node, &server_dest_hash).await;

    if !establish_link(&node, &mux, &server_dest_hash, sig_pub_bytes, &mut rx).await {
        return;
    }

    // Start SOCKS5 listener
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind SOCKS5 port: {}", e);
            return;
        }
    };
    info!("SOCKS5 ready: 127.0.0.1:{}", port);

    // Notify used to signal the accept loop that the link was lost and reconnected
    let reconnect_notify = Arc::new(Notify::new());

    // Spawn event dispatch + reconnection task
    let mux_dispatch = mux.clone();
    let node_reconn = Arc::clone(&node);
    let reconnect_notify_clone = Arc::clone(&reconnect_notify);
    tokio::spawn(async move {
        dispatch_and_reconnect(
            mux_dispatch,
            node_reconn,
            server_dest_hash,
            rx,
            reconnect_notify_clone,
        )
        .await;
    });

    // Accept SOCKS5 connections
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _addr) = match accept_result {
                    Ok(sa) => sa,
                    Err(e) => {
                        warn!("Accept error: {}", e);
                        continue;
                    }
                };

                if !mux.is_connected() {
                    warn!("No RNS link, rejecting connection");
                    drop(stream);
                    continue;
                }

                let sid = mux.next_session_id();
                let session_rx = mux.register_session(sid);
                let mux_clone = mux.clone();

                tokio::spawn(async move {
                    handle_socks5_session(sid, stream, mux_clone, session_rx).await;
                });
            }
            _ = reconnect_notify.notified() => {
                // Link was re-established, just continue accepting
                info!("SOCKS5 ready: 127.0.0.1:{}", port);
            }
        }
    }
}

/// Timeout for waiting for a link to be established.
///
/// WORKAROUND(rns-rs#3): LocalClientInterface has no reconnect logic, so when
/// rnsd restarts the interface goes offline permanently and create_link packets
/// are silently dropped. Without this timeout establish_link would hang forever.
/// Remove once rns-rs merges LocalClientInterface reconnection support.
/// https://github.com/lelloman/rns-rs/issues/3
const LINK_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(15);

/// Establish an RNS link, waiting for the LinkEstablished event.
///
/// On success, sets the link id on the mux and returns `true`.
/// Times out after `LINK_ESTABLISH_TIMEOUT` to avoid hanging forever
/// when the underlying transport is dead (e.g. rnsd was restarted).
async fn establish_link(
    node: &RnsNode,
    mux: &MuxHandle,
    dest_hash: &[u8; 16],
    sig_pub_bytes: [u8; 32],
    rx: &mut mpsc::UnboundedReceiver<ProxyEvent>,
) -> bool {
    info!("Establishing link...");
    let link_id = match node.create_link(*dest_hash, sig_pub_bytes) {
        Ok(id) => LinkId::from(id),
        Err(e) => {
            error!("Failed to create link: {:?}", e);
            return false;
        }
    };

    let result = tokio::time::timeout(LINK_ESTABLISH_TIMEOUT, async {
        loop {
            let event = match rx.recv().await {
                Some(e) => e,
                None => {
                    error!("Event channel closed");
                    return false;
                }
            };

            match event {
                ProxyEvent::LinkEstablished {
                    link_id: lid,
                    rtt,
                    is_initiator,
                } => {
                    if lid == link_id && is_initiator {
                        info!("RNS link established (rtt={:.1}ms)", rtt * 1000.0);
                        mux.set_link_id(link_id);
                        return true;
                    }
                }
                ProxyEvent::LinkClosed {
                    link_id: lid,
                    reason,
                } => {
                    if lid == link_id {
                        error!("Link closed during setup: {:?}", reason);
                        return false;
                    }
                }
                _ => {}
            }
        }
    })
    .await;

    match result {
        Ok(success) => success,
        Err(_) => {
            warn!("Link establishment timed out (transport may be down)");
            false
        }
    }
}

/// Maximum consecutive link establishment failures before recreating the RNS node.
///
/// WORKAROUND(rns-rs#3): LocalClientInterface has no reconnect logic. When rnsd
/// restarts, the interface goes offline permanently and packets are silently
/// dropped. After this many consecutive establish_link timeouts, we recreate
/// the entire RnsNode to get a fresh TCP connection to rnsd.
/// Remove once rns-rs merges LocalClientInterface reconnection support.
/// https://github.com/lelloman/rns-rs/issues/3
const MAX_LINK_FAILURES_BEFORE_NODE_RECREATE: u32 = 3;

/// Event dispatch loop with automatic reconnection.
///
/// Reads RNS events, dispatches channel messages to sessions, and when the
/// link is lost, waits briefly and re-establishes it.
///
/// WORKAROUND(rns-rs#3): If the underlying transport is dead (rnsd restarted),
/// link establishment will repeatedly time out. After several failures, the
/// RNS node is recreated to get a fresh connection to rnsd. The entire node
/// recreation block (InterfaceDown handling + consecutive_failures +
/// create_node loop) can be removed once rns-rs merges LocalClientInterface
/// reconnection support — at that point a simple retry of establish_link
/// will suffice.
/// https://github.com/lelloman/rns-rs/issues/3
async fn dispatch_and_reconnect(
    mux: MuxHandle,
    mut node: Arc<RnsNode>,
    dest_hash: [u8; 16],
    mut rx: mpsc::UnboundedReceiver<ProxyEvent>,
    reconnect_notify: Arc<Notify>,
) {
    let mut transport_dead = false;

    loop {
        // --- Dispatch phase: forward channel messages to sessions ---
        if !transport_dead {
            loop {
                let event = match rx.recv().await {
                    Some(e) => e,
                    None => {
                        error!("Event channel closed, shutting down");
                        return;
                    }
                };

                match event {
                    ProxyEvent::LinkData { data, .. } => {
                        for frame in mux.receive_data(&data) {
                            mux.dispatch(frame);
                        }
                    }
                    ProxyEvent::LinkClosed { link_id, reason } => {
                        warn!("Connection lost (link={}, reason={:?})", link_id, reason);
                        mux.reset();
                        break; // Exit dispatch loop to reconnect
                    }
                    // WORKAROUND(rns-rs#3): LocalClientInterface has no reconnect,
                    // so we must recreate the entire node. Remove once fixed upstream.
                    // https://github.com/lelloman/rns-rs/issues/3
                    ProxyEvent::InterfaceDown => {
                        warn!("Transport to rnsd lost");
                        mux.reset();
                        transport_dead = true;
                        break; // Skip to node recreation
                    }
                    _ => {}
                }
            }
        }

        // --- Reconnection phase ---
        let mut delay = 1u64;
        let mut consecutive_failures: u32 = if transport_dead {
            // Skip straight to node recreation
            MAX_LINK_FAILURES_BEFORE_NODE_RECREATE
        } else {
            0
        };
        transport_dead = false;

        loop {
            // WORKAROUND(rns-rs#3): LocalClientInterface has no reconnect logic.
            // If too many consecutive failures, the transport is likely dead —
            // recreate the RNS node to get a fresh connection to rnsd.
            // This entire block can be removed once rns-rs merges the fix.
            // https://github.com/lelloman/rns-rs/issues/3
            if consecutive_failures >= MAX_LINK_FAILURES_BEFORE_NODE_RECREATE {
                warn!(
                    "Transport appears dead after {} failures, recreating RNS node...",
                    consecutive_failures
                );

                match create_node() {
                    Ok((new_node, new_rx)) => {
                        info!("RNS node recreated, new connection to rnsd");
                        node = new_node;
                        rx = new_rx;
                        mux.replace_node(Arc::clone(&node));
                        consecutive_failures = 0;
                        delay = 1;
                        // After recreating the node, paths are gone.
                        // Fall through to path discovery below.
                    }
                    Err(e) => {
                        warn!("Failed to recreate RNS node: {} (rnsd may not be running)", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        // Keep trying to recreate — don't reset consecutive_failures
                        continue;
                    }
                }
            }

            info!("Reconnecting in {}s...", delay);
            tokio::time::sleep(Duration::from_secs(delay)).await;

            // Make sure path is still valid
            if !ensure_path(&node, &dest_hash, 15).await {
                warn!("Path not found, will retry...");
                delay = (delay * 2).min(30);
                consecutive_failures += 1;
                continue;
            }

            // Refresh signing public key — it may have changed if the server
            // was restarted and re-announced with a new identity.
            let sig_pub_bytes = match recall_sig_pub(&node, &dest_hash) {
                Some(sig_pub) => sig_pub,
                None => {
                    warn!("Failed to recall identity, will retry...");
                    delay = (delay * 2).min(30);
                    consecutive_failures += 1;
                    continue;
                }
            };

            if establish_link(&node, &mux, &dest_hash, sig_pub_bytes, &mut rx).await {
                info!("Reconnected successfully");
                reconnect_notify.notify_one();
                break; // Back to dispatch phase
            }

            warn!("Reconnection failed, will retry...");
            delay = (delay * 2).min(30);
            consecutive_failures += 1;
        }
    }
}

/// Handle a single SOCKS5 client session using fast-socks5.
async fn handle_socks5_session(
    sid: u32,
    stream: tokio::net::TcpStream,
    mux: MuxHandle,
    mut session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    // --- SOCKS5 handshake via fast-socks5 ---
    let proto = match Socks5ServerProtocol::accept_no_auth(stream).await {
        Ok(p) => p,
        Err(e) => {
            debug!("[{}] SOCKS5 auth handshake failed: {}", sid, e);
            return;
        }
    };

    let (proto, cmd, target_addr) = match proto.read_command().await {
        Ok(result) => result,
        Err(e) => {
            debug!("[{}] SOCKS5 read_command failed: {}", sid, e);
            return;
        }
    };

    // Only support TCP CONNECT
    if cmd != Socks5Command::TCPConnect {
        let _ = proto.reply_error(&ReplyError::CommandNotSupported).await;
        return;
    }

    // Extract host and port from TargetAddr
    let (host, port) = target_addr.into_string_and_port();

    info!("[{}] -> {}:{}", sid, host, port);

    // Send CONNECT frame through RNS
    let connect_payload = encode_connect_payload(&host, port);
    mux.send(FrameType::Connect, sid, connect_payload);

    // Wait for CONN_OK or CONN_ERR with timeout
    let connect_result = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(frame) = session_rx.recv().await {
            match frame.frame_type {
                FrameType::ConnectOk => return Ok(()),
                FrameType::ConnectErr => {
                    let reason = String::from_utf8_lossy(&frame.payload).to_string();
                    return Err(reason);
                }
                _ => continue,
            }
        }
        Err("channel closed".to_string())
    })
    .await;

    // Reply to SOCKS5 client based on RNS connection result
    let dummy_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0);

    let stream = match connect_result {
        Ok(Ok(())) => {
            // Connection succeeded -- send SOCKS5 success reply
            match proto.reply_success(dummy_addr).await {
                Ok(s) => s,
                Err(e) => {
                    debug!("[{}] Failed to send SOCKS5 reply: {}", sid, e);
                    mux.send(FrameType::Close, sid, Vec::new());
                    mux.drop_session(sid);
                    return;
                }
            }
        }
        Ok(Err(reason)) => {
            warn!("[{}] Remote connect failed: {}", sid, reason);
            let _ = proto.reply_error(&ReplyError::GeneralFailure).await;
            mux.drop_session(sid);
            return;
        }
        Err(_) => {
            warn!("[{}] Connect timeout", sid);
            let _ = proto.reply_error(&ReplyError::TtlExpired).await;
            mux.drop_session(sid);
            return;
        }
    };

    // Data relay (shared implementation)
    relay_bidirectional(sid, stream, mux, session_rx).await;
}

/// Wait for a path to the server, then recall the identity and return sig_pub_bytes.
async fn wait_for_path(node: &RnsNode, dest_hash: &[u8; 16]) -> [u8; 32] {
    ensure_path(node, dest_hash, 30).await;

    // Recall identity (retry until available)
    loop {
        if let Some(sig_pub) = recall_sig_pub(node, dest_hash) {
            info!("Path found, identity recalled");
            return sig_pub;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

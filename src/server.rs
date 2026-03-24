//! RNS SOCKS5 server -- accepts incoming RNS links, proxies TCP connections.
//!
//! Equivalent of `rns_socks_server.py`.
//!
//! The server:
//! 1. Creates an RNS identity and registers a link destination.
//! 2. Periodically announces itself.
//! 3. On each incoming link, creates a `MuxHandle`.
//! 4. For each CONNECT frame, spawns a tokio task that opens a real TCP connection
//!    and relays data bidirectionally.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{error, info, warn};
use rns_crypto::identity::Identity;
use rns_net::storage;
use rns_net::{Destination, IdentityHash, LinkId};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::mux::MuxHandle;
use crate::{
    create_node, decode_connect_payload, relay_bidirectional, Frame, FrameType, ProxyEvent,
    APP_ASPECT, APP_NAME,
};

/// Default identity filename inside the Reticulum config directory.
const DEFAULT_IDENTITY_FILENAME: &str = "rns_proxy_identity";

/// Resolve the identity file path.
///
/// If `override_path` is given, uses it as-is.  Otherwise defaults to
/// `~/.reticulum/<DEFAULT_IDENTITY_FILENAME>`.
fn identity_file_path(override_path: Option<&str>) -> PathBuf {
    match override_path {
        Some(p) => PathBuf::from(p),
        None => storage::resolve_config_dir(None).join(DEFAULT_IDENTITY_FILENAME),
    }
}

/// Run the SOCKS5 server.
///
/// `identity_path` overrides the default identity file location
/// (`~/.reticulum/rns_proxy_identity`).
pub async fn run_server(identity_path: Option<&str>) {
    let id_path = identity_file_path(identity_path);

    let identity = if id_path.exists() {
        let id = storage::load_identity(&id_path).unwrap_or_else(|e| {
            panic!("Failed to load identity from {}: {}", id_path.display(), e);
        });
        info!("Loaded identity from {}", id_path.display());
        id
    } else {
        let id = Identity::new(&mut rns_crypto::OsRng);
        if let Some(parent) = id_path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!("Failed to create directory {}: {}", parent.display(), e);
            });
        }
        storage::save_identity(&id, &id_path).unwrap_or_else(|e| {
            panic!("Failed to save identity to {}: {}", id_path.display(), e);
        });
        info!("Generated new identity, saved to {}", id_path.display());
        id
    };

    let identity_prv_bytes = identity.get_private_key().expect("has private key");
    let dest = Destination::single_in(APP_NAME, &[APP_ASPECT], IdentityHash(*identity.hash()));
    let dest_hash = dest.hash.0;

    // Get signing keys for link destination registration
    let prv_key = identity.get_private_key().expect("identity has private key");
    let pub_key = identity.get_public_key().expect("identity has public key");
    let sig_prv: [u8; 32] = prv_key[32..64].try_into().unwrap();
    let sig_pub: [u8; 32] = pub_key[32..64].try_into().unwrap();

    info!("Server address (stable across restarts):");
    info!("  {}", hex::encode(dest_hash));

    let (node, mut rx) = match create_node() {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to create RNS node: {}", e);
            return;
        }
    };

    // Register link destination (server accepts incoming links)
    if let Err(e) = node.register_link_destination(dest_hash, sig_prv, sig_pub, 0) {
        error!("Failed to register link destination: {:?}", e);
        return;
    }

    let id = Identity::from_private_key(&identity_prv_bytes);
    if let Err(e) = node.announce(&dest, &id, None) {
        warn!("Failed to send announce: {:?}", e);
    }
    info!("Server ready, waiting for connections...");

    // Per-link state
    let link_muxes: Arc<Mutex<std::collections::HashMap<LinkId, MuxHandle>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Periodic announce task
    let node_announce = Arc::clone(&node);
    let dest_clone = dest.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let id = Identity::from_private_key(&identity_prv_bytes);
            if let Err(e) = node_announce.announce(&dest_clone, &id, None) {
                warn!("Failed to send periodic announce: {:?}", e);
            }
        }
    });

    // Event loop
    loop {
        let event = match rx.recv().await {
            Some(e) => e,
            None => return,
        };

        match event {
            ProxyEvent::LinkEstablished {
                link_id,
                rtt,
                is_initiator,
            } => {
                if is_initiator {
                    continue; // We only care about incoming links
                }
                info!(
                    "New client connection (link={}, rtt={:.1}ms)",
                    link_id,
                    rtt * 1000.0
                );

                let mux = MuxHandle::new(Arc::clone(&node));
                mux.set_link_id(link_id);

                link_muxes.lock().unwrap().insert(link_id, mux);
            }

            ProxyEvent::LinkClosed { link_id, .. } => {
                info!("Client disconnected (link={})", link_id);
                link_muxes.lock().unwrap().remove(&link_id);
            }

            ProxyEvent::LinkData { link_id, data } => {
                let mux = {
                    let muxes = link_muxes.lock().unwrap();
                    match muxes.get(&link_id) {
                        Some(m) => m.clone(),
                        None => continue,
                    }
                };

                for frame in mux.receive_data(&data) {
                    match frame.frame_type {
                        FrameType::Connect => {
                            let sid = frame.session_id;
                            if let Some((host, port)) = decode_connect_payload(&frame.payload) {
                                info!("[{}] -> {}:{}", sid, host, port);
                                let session_rx = mux.register_session(sid);
                                let mux_clone = mux.clone();
                                tokio::spawn(async move {
                                    handle_server_session(sid, host, port, mux_clone, session_rx)
                                        .await;
                                });
                            } else {
                                warn!("[{}] Invalid CONNECT payload", sid);
                                mux.send(
                                    FrameType::ConnectErr,
                                    sid,
                                    b"invalid payload".to_vec(),
                                );
                            }
                        }
                        FrameType::Data | FrameType::Close => {
                            mux.dispatch(frame);
                        }
                        _ => {}
                    }
                }
            }

            _ => {}
        }
    }
}

/// Handle a single proxied TCP session on the server side.
async fn handle_server_session(
    sid: u32,
    host: String,
    port: u16,
    mux: MuxHandle,
    session_rx: mpsc::UnboundedReceiver<Frame>,
) {
    // Attempt TCP connection
    let stream = match TcpStream::connect(format!("{}:{}", host, port)).await {
        Ok(s) => s,
        Err(e) => {
            warn!("[{}] Connection failed: {}", sid, e);
            mux.send(FrameType::ConnectErr, sid, e.to_string().into_bytes());
            mux.drop_session(sid);
            return;
        }
    };

    // Signal success
    mux.send(FrameType::ConnectOk, sid, Vec::new());

    // Data relay (shared implementation)
    relay_bidirectional(sid, stream, mux, session_rx).await;
    info!("[{}] Closed", sid);
}

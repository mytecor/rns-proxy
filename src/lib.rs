//! RNS SOCKS5 proxy — shared types, protocol, and helpers.
//!
//! All TCP sessions are multiplexed over a single RNS link. Each session gets
//! a unique `session_id`. Data is exchanged as frames:
//!
//! ```text
//! [1 byte type][4 bytes session_id][2 bytes payload length][payload]
//! ```

pub mod client;
pub mod frame;
pub mod mux;
pub mod node;
pub mod relay;
pub mod server;

// Re-export commonly used items so existing `use crate::*` still works.
pub use frame::{decode_connect_payload, encode_connect_payload, Frame, FrameType};
pub use node::{create_node, ProxyEvent};
pub use relay::relay_bidirectional;

use log::info;
use rns_net::{DestHash, RnsNode};

// ---------------------------------------------------------------------------
// RNS application identity
// ---------------------------------------------------------------------------

pub const APP_NAME: &str = "rns_socks";
pub const APP_ASPECT: &str = "proxy";

// ---------------------------------------------------------------------------
// Shared path discovery helpers
// ---------------------------------------------------------------------------

/// Ensure a path to the destination exists, requesting one if needed.
///
/// Returns `true` if a path is available, `false` if the path was not found
/// after `timeout_secs` seconds of polling.
pub async fn ensure_path(node: &RnsNode, dest_hash: &[u8; 16], timeout_secs: u32) -> bool {
    let dh = DestHash(*dest_hash);

    if node.has_path(&dh).unwrap_or(false) {
        return true;
    }

    info!("Requesting path...");
    let _ = node.request_path(&dh);

    for _ in 0..timeout_secs {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if node.has_path(&dh).unwrap_or(false) {
            return true;
        }
    }

    false
}

/// Recall the signing public key for a destination.
///
/// Returns the 32-byte Ed25519 public key on success, or `None` if the
/// identity could not be recalled.
pub fn recall_sig_pub(node: &RnsNode, dest_hash: &[u8; 16]) -> Option<[u8; 32]> {
    let dh = DestHash(*dest_hash);
    match node.recall_identity(&dh) {
        Ok(Some(recalled)) => {
            let sig_pub: [u8; 32] = recalled.public_key[32..64].try_into().unwrap();
            Some(sig_pub)
        }
        _ => None,
    }
}

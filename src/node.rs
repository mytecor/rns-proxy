//! RNS node initialization and event callbacks.
//!
//! Provides a shared `create_node()` factory and the unified `ProxyEvent` /
//! `ProxyCallbacks` types used by both client and server.

use std::sync::Arc;

use rns_net::common::destination::AnnouncedIdentity;
use rns_net::{
    Callbacks, DestHash, InterfaceId, LinkId, PacketHash, RnsNode, SharedClientConfig,
    TeardownReason,
};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Node initialization
// ---------------------------------------------------------------------------

/// Create an RNS node connected to the local daemon in shared-client mode.
///
/// Returns the node handle and the event receiver for proxy events.
pub fn create_node() -> Result<(Arc<RnsNode>, mpsc::UnboundedReceiver<ProxyEvent>), String> {
    let (tx, rx) = mpsc::unbounded_channel();
    let callbacks = Box::new(ProxyCallbacks { tx });
    let node = RnsNode::connect_shared(SharedClientConfig::default(), callbacks)
        .map_err(|e| format!("Failed to start RNS node: {}", e))?;
    Ok((Arc::new(node), rx))
}

// ---------------------------------------------------------------------------
// Proxy events & callbacks
// ---------------------------------------------------------------------------

/// Unified event type for both client and server sides.
#[derive(Debug)]
pub enum ProxyEvent {
    LinkEstablished {
        link_id: LinkId,
        rtt: f64,
        is_initiator: bool,
    },
    LinkClosed {
        link_id: LinkId,
        reason: Option<TeardownReason>,
    },
    LinkData {
        link_id: LinkId,
        data: Vec<u8>,
    },
    PathUpdated,
    /// The underlying interface (connection to rnsd) went offline.
    ///
    /// WORKAROUND(rns-rs#3): Used to detect rnsd restart since LocalClientInterface
    /// has no reconnect logic. Can be removed once fixed upstream — the interface
    /// will reconnect transparently and this event won't need special handling.
    /// https://github.com/lelloman/rns-rs/issues/3
    InterfaceDown,
}

/// Shared RNS callbacks that forward events via an mpsc channel.
pub struct ProxyCallbacks {
    pub tx: mpsc::UnboundedSender<ProxyEvent>,
}

impl Callbacks for ProxyCallbacks {
    fn on_announce(&mut self, _announced: AnnouncedIdentity) {}

    fn on_path_updated(&mut self, _dest_hash: DestHash, _hops: u8) {
        let _ = self.tx.send(ProxyEvent::PathUpdated);
    }

    fn on_local_delivery(&mut self, _dest_hash: DestHash, _raw: Vec<u8>, _packet_hash: PacketHash) {
    }

    fn on_link_established(
        &mut self,
        link_id: LinkId,
        _dest_hash: DestHash,
        rtt: f64,
        is_initiator: bool,
    ) {
        let _ = self.tx.send(ProxyEvent::LinkEstablished {
            link_id,
            rtt,
            is_initiator,
        });
    }

    fn on_link_closed(&mut self, link_id: LinkId, reason: Option<TeardownReason>) {
        let _ = self.tx.send(ProxyEvent::LinkClosed { link_id, reason });
    }

    fn on_link_data(&mut self, link_id: LinkId, _context: u8, data: Vec<u8>) {
        let _ = self.tx.send(ProxyEvent::LinkData { link_id, data });
    }

    /// WORKAROUND(rns-rs#3): Forward interface-down to trigger node recreation.
    /// Remove once LocalClientInterface reconnection is fixed upstream.
    /// https://github.com/lelloman/rns-rs/issues/3
    fn on_interface_down(&mut self, _id: InterfaceId) {
        let _ = self.tx.send(ProxyEvent::InterfaceDown);
    }
}

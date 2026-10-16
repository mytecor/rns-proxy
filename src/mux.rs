//! Multiplexer -- manages sessions over a single RNS link.
//!
//! All TCP sessions are multiplexed over one RNS link per peer. Each session
//! gets a unique `session_id`. Data is exchanged as frames:
//!
//! ```text
//! [1 byte type][4 bytes session_id][2 bytes payload length][payload]
//! ```
//!
//! Frames are sent as raw encrypted link data via `send_on_link` (CONTEXT_NONE).
//! Frames larger than LINK_MDU are split into LINK_MDU-sized chunks; the
//! receiver reassembles them using the frame's embedded length header.
//!
//! We do NOT use the RNS Channel API because its ACK path is not wired up
//! in rns-rs, causing `NotReady` errors after the first 2 messages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use log::{debug, warn};
use rns_core::constants::LINK_MDU;
use rns_net::{LinkId, RnsNode};
use tokio::sync::mpsc;

use crate::{Frame, FrameDecodeError, FrameType};

/// Context byte for our link data. We use CONTEXT_NONE (0x00) which routes
/// to the `on_link_data` callback on the receive side.
const DATA_CONTEXT: u8 = 0x00;

/// A handle that session tasks use to send frames back through the RNS link.
#[derive(Clone)]
pub struct MuxHandle {
    inner: Arc<MuxInner>,
}

struct MuxInner {
    _node: Option<Arc<RnsNode>>,
    link_id: Arc<Mutex<Option<LinkId>>>,
    sessions: Mutex<HashMap<u32, mpsc::UnboundedSender<Frame>>>,
    next_sid: Mutex<u32>,
    /// Reassembly buffer for incoming raw link data chunks.
    recv_buf: Mutex<Vec<u8>>,
    frame_tx: mpsc::Sender<Frame>,
}

impl MuxHandle {
    /// Create a new multiplexer handle and spawn its background frame sender.
    pub fn new(node: Arc<RnsNode>) -> Self {
        let link_id: Arc<Mutex<Option<LinkId>>> = Arc::new(Mutex::new(None));
        let (frame_tx, mut frame_rx) = mpsc::channel::<Frame>(32);

        let link_id_sender = link_id.clone();
        let node_sender = node.clone();

        tokio::spawn(async move {
            while let Some(frame) = frame_rx.recv().await {
                let active_link = match *link_id_sender.lock().unwrap() {
                    Some(id) => id,
                    None => {
                        debug!("send_frame: no active link, dropping frame");
                        continue;
                    }
                };

                let encoded = frame.encode();
                for chunk in encoded.chunks(LINK_MDU) {
                    if let Err(e) = node_sender.send_on_link(active_link.0, chunk.to_vec(), DATA_CONTEXT).await {
                        warn!("Failed to send link data: {:?}", e);
                        *link_id_sender.lock().unwrap() = None;
                        break;
                    }
                }
            }
        });

        Self {
            inner: Arc::new(MuxInner {
                _node: Some(node),
                link_id,
                sessions: Mutex::new(HashMap::new()),
                next_sid: Mutex::new(0),
                recv_buf: Mutex::new(Vec::new()),
                frame_tx,
            }),
        }
    }

    /// Create a test/mock multiplexer handle disconnected from Reticulum.
    pub fn new_test() -> (Self, mpsc::Receiver<Frame>) {
        let link_id: Arc<Mutex<Option<LinkId>>> = Arc::new(Mutex::new(None));
        let (frame_tx, frame_rx) = mpsc::channel::<Frame>(32);

        (
            Self {
                inner: Arc::new(MuxInner {
                    _node: None,
                    link_id,
                    sessions: Mutex::new(HashMap::new()),
                    next_sid: Mutex::new(0),
                    recv_buf: Mutex::new(Vec::new()),
                    frame_tx,
                }),
            },
            frame_rx,
        )
    }

    /// Set the active link id (called when the link is established).
    pub fn set_link_id(&self, link_id: LinkId) {
        *self.inner.link_id.lock().unwrap() = Some(link_id);
    }

    /// Clear the link id (called when the link is closed).
    pub fn clear_link_id(&self) {
        *self.inner.link_id.lock().unwrap() = None;
    }

    /// Reset the mux for a new link (clears sessions and reassembly buffer).
    ///
    /// Called on reconnection.
    pub fn reset(&self) {
        *self.inner.link_id.lock().unwrap() = None;
        self.inner.sessions.lock().unwrap().clear();
        self.inner.recv_buf.lock().unwrap().clear();
    }

    /// Check if link is active.
    pub fn is_connected(&self) -> bool {
        self.inner.link_id.lock().unwrap().is_some()
    }

    /// Get the next session id.
    pub fn next_session_id(&self) -> u32 {
        let mut sid = self.inner.next_sid.lock().unwrap();
        *sid = sid.wrapping_add(1);
        *sid
    }

    /// Register a session. Returns a receiver for frames addressed to this session.
    pub fn register_session(&self, sid: u32) -> mpsc::UnboundedReceiver<Frame> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.sessions.lock().unwrap().insert(sid, tx);
        rx
    }

    /// Remove a session.
    pub fn drop_session(&self, sid: u32) {
        self.inner.sessions.lock().unwrap().remove(&sid);
    }

    /// Send a frame over the RNS link via the dedicated sender worker.
    ///
    /// All chunks of the frame are sent consecutively without interleaving from
    /// other sessions. The bounded sender channel provides backpressure to the
    /// TCP reader tasks.
    pub async fn send_frame(&self, frame: &Frame) {
        if let Err(e) = self.inner.frame_tx.send(frame.clone()).await {
            debug!("send_frame: sender channel closed: {}", e);
        }
    }

    /// Convenience: send a typed frame.
    pub async fn send(&self, frame_type: FrameType, session_id: u32, payload: Vec<u8>) {
        self.send_frame(&Frame::new(frame_type, session_id, payload)).await;
    }

    /// Dispatch an incoming frame to the appropriate session.
    pub fn dispatch(&self, frame: Frame) {
        let sid = frame.session_id;
        let ft = frame.frame_type;
        let sessions = self.inner.sessions.lock().unwrap();
        if let Some(tx) = sessions.get(&sid) {
            if tx.send(frame).is_err() {
                debug!("Session {} channel closed", sid);
            }
        } else {
            debug!("No session {} for frame type {}", sid, ft);
        }
    }

    /// Feed raw link data and extract any complete frames.
    ///
    /// Called when `on_link_data` fires. The data may be a partial chunk of a
    /// larger frame, so we buffer and try to decode complete frames.
    /// If frame corruption occurs, the corrupted buffer prefix is discarded
    /// rather than permanently stalling the pipeline.
    pub fn receive_data(&self, data: &[u8]) -> Vec<Frame> {
        let mut buf = self.inner.recv_buf.lock().unwrap();
        buf.extend_from_slice(data);

        let mut frames = Vec::new();
        loop {
            match Frame::decode(&buf) {
                Ok((frame, consumed)) => {
                    buf.drain(..consumed);
                    frames.push(frame);
                }
                Err(FrameDecodeError::Incomplete) => break,
                Err(FrameDecodeError::Corrupted) => {
                    log::error!(
                        "Frame decode corrupted (invalid header). Dropping {} buffered bytes (prefix: {:?})",
                        buf.len(),
                        &buf[..buf.len().min(16)]
                    );
                    buf.clear();
                    break;
                }
            }
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mux_receive_data_chunks_reassembly() {
        let (mux, _rx) = MuxHandle::new_test();
        let payload = vec![0x42; 100];
        let frame = Frame::new(FrameType::Data, 1, payload.clone());
        let encoded = frame.encode();

        // Feed chunk 1
        let frames1 = mux.receive_data(&encoded[..20]);
        assert!(frames1.is_empty());

        // Feed chunk 2
        let frames2 = mux.receive_data(&encoded[20..60]);
        assert!(frames2.is_empty());

        // Feed chunk 3
        let frames3 = mux.receive_data(&encoded[60..]);
        assert_eq!(frames3.len(), 1);
        assert_eq!(frames3[0].frame_type, FrameType::Data);
        assert_eq!(frames3[0].session_id, 1);
        assert_eq!(frames3[0].payload, payload);
    }

    #[test]
    fn test_mux_receive_data_multiple_frames() {
        let (mux, _rx) = MuxHandle::new_test();
        let f1 = Frame::new(FrameType::ConnectOk, 1, vec![]);
        let f2 = Frame::new(FrameType::Data, 1, b"hello".to_vec());

        let mut data = f1.encode();
        data.extend_from_slice(&f2.encode());

        let frames = mux.receive_data(&data);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].frame_type, FrameType::ConnectOk);
        assert_eq!(frames[1].frame_type, FrameType::Data);
        assert_eq!(frames[1].payload, b"hello");
    }

    #[test]
    fn test_mux_receive_data_corrupted_recovery() {
        let (mux, _rx) = MuxHandle::new_test();
        let f = Frame::new(FrameType::Data, 2, b"valid".to_vec());

        // Send garbage first (invalid frame type 0xFF)
        let garbage = vec![0xFF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
        let frames1 = mux.receive_data(&garbage);
        assert!(frames1.is_empty());

        // Now send a valid frame; mux buffer should have been cleared and recovers cleanly
        let frames2 = mux.receive_data(&f.encode());
        assert_eq!(frames2.len(), 1);
        assert_eq!(frames2[0].session_id, 2);
        assert_eq!(frames2[0].payload, b"valid");
    }

    #[tokio::test]
    async fn test_mux_dispatch_and_send() {
        let (mux, mut frame_rx) = MuxHandle::new_test();
        let mut session_rx = mux.register_session(10);

        // Dispatch incoming frame
        let in_frame = Frame::new(FrameType::Data, 10, b"to-client".to_vec());
        mux.dispatch(in_frame.clone());

        let received = session_rx.recv().await.expect("should receive dispatched frame");
        assert_eq!(received.session_id, 10);
        assert_eq!(received.payload, b"to-client");

        // Send outgoing frame
        mux.send(FrameType::Data, 10, b"to-link".to_vec()).await;
        let sent = frame_rx.recv().await.expect("sender should receive frame");
        assert_eq!(sent.session_id, 10);
        assert_eq!(sent.payload, b"to-link");

        // Drop session
        mux.drop_session(10);
    }
}

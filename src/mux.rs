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
use std::sync::Arc;
use tokio::sync::Mutex;

use log::{error, info, warn};
use rns_core::constants::LINK_MDU;
use rns_net::{LinkId, RnsNode};
use tokio::sync::mpsc::{self, channel, Receiver, Sender};

use crate::frame::FrameDecodeState::{DecodingFailed, MoreDataRequired};
use crate::{Frame, FrameType};

/// Context byte for our link data. We use CONTEXT_NONE (0x00) which routes
/// to the `on_link_data` callback on the receive side.
const DATA_CONTEXT: u8 = 0x00;

/// A handle that session tasks use to send frames back through the RNS link.
#[derive(Clone)]
pub struct MuxHandle {
    inner: Arc<MuxInner>,
}

// #[ignore(unused_attributes)]
struct MuxInner {
    _node: Arc<RnsNode>, // we migth use this someday but right now the run_link_sender thread is the only thing that uses it.
    link_id: Arc<Mutex<Option<LinkId>>>,
    sessions: Mutex<HashMap<u32, tokio::sync::mpsc::UnboundedSender<Frame>>>,
    next_sid: Mutex<u32>,
    /// Reassembly buffer for incoming raw link data chunks.
    recv_buf: Mutex<Vec<u8>>,
    data_sender_buf: Arc<Sender<Vec<u8>>>,
}

// allows for sending things faster cause it's on a different thread and makes sure everything ends up in order.
pub fn run_link_sender(node: Arc<RnsNode>, link_id: Arc<Mutex<Option<LinkId>>>) -> Sender<Vec<u8>> {
    let (sender, mut receiver): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel(5);

    tokio::spawn(async move {
        while let Some(data_frame) = receiver.recv().await {
            let active_link = match *link_id.lock().await {
                Some(id) => id,
                None => {
                    warn!("send_frame: no active link, dropping frame");
                    continue;
                }
            };

            for chunk in data_frame.chunks(LINK_MDU) {
                println!("{:?}", chunk);
                if let Err(e) = node
                    .send_on_link(active_link.0, chunk.to_vec(), DATA_CONTEXT)
                    .await
                {
                    warn!("Failed to send link data: {:?}", e);
                    *link_id.lock().await = None;
                    break;
                }
            }
            println!("{}", receiver.len());
        }
    });

    return sender;
}

impl MuxHandle {
    /// Create a new multiplexer handle.
    pub fn new(node: Arc<RnsNode>) -> Self {
        let link_id = Arc::new(Mutex::new(None));
        Self {
            inner: Arc::new(MuxInner {
                _node: node.clone(),
                link_id: link_id.clone(),
                sessions: Mutex::new(HashMap::new()),
                next_sid: Mutex::new(0),
                recv_buf: Mutex::new(Vec::new()),
                data_sender_buf: Arc::new(run_link_sender(node.clone(), link_id)),
            }),
        }
    }

    /// Set the active link id (called when the link is established).
    pub async fn set_link_id(&self, link_id: LinkId) {
        *(self.inner.link_id.lock().await) = Some(link_id);
    }

    /// Clear the link id (called when the link is closed).
    pub async fn clear_link_id(&self) {
        *(self.inner.link_id.lock().await) = None;
    }

    /// Reset the mux for a new link (clears sessions and reassembly buffer).
    ///
    /// Called on reconnection.
    pub async fn reset(&self) {
        *(self.inner.link_id.lock().await) = None;
        (self.inner.sessions.lock().await).clear();
        (self.inner.recv_buf.lock().await).clear();
    }

    /// Check if link is active.
    pub async fn is_connected(&self) -> bool {
        (self.inner.link_id.lock().await).is_some()
    }

    /// Get the next session id.
    pub async fn next_session_id(&self) -> u32 {
        let mut sid = self.inner.next_sid.lock().await;
        *sid = sid.wrapping_add(1);
        *sid
    }

    /// Register a session. Returns a receiver for frames addressed to this session.
    pub async fn register_session(&self, sid: u32) -> mpsc::UnboundedReceiver<Frame> {
        let (tx, rx) = mpsc::unbounded_channel();
        (self.inner.sessions.lock().await).insert(sid, tx);
        rx
    }

    /// Remove a session.
    pub async fn drop_session(&self, sid: u32) {
        (self.inner.sessions.lock().await).remove(&sid);
    }

    /// Send a frame over the RNS link.
    ///
    /// The encoded frame is split into chunks of at most `LINK_MDU` bytes and
    /// each chunk is sent as a separate `send_on_link` call with `CONTEXT_NONE`.
    /// The receiver reassembles using the frame length header.
    ///
    /// Note that we are using .inner.link_id.lock as the guard to prevent
    /// multiple different sids from sending at the same time and scrambling packets
    pub async fn send_frame(&self, frame: &Frame) {
        let encoded = frame.encode();
        _ = self.inner.data_sender_buf.send(encoded).await;
        // pretty much should never error so we don't care.
    }

    /// Convenience: send a typed frame.
    pub async fn send(&self, frame_type: FrameType, session_id: u32, payload: Vec<u8>) {
        // info!("send frame");
        self.send_frame(&Frame::new(frame_type, session_id, payload))
            .await;
    }

    /// Dispatch an incoming frame to the appropriate session.
    pub async fn dispatch(&self, frame: Frame) {
        let sid = frame.session_id;
        let ft = frame.frame_type;

        let sessions = self.inner.sessions.lock().await;
        if let Some(tx) = sessions.get(&sid) {
            // info!("hi");
            if tx.send(frame).is_err() {
                // debug!("Session {} channel closed", sid);
            } else {
                // info!("fine?");
            }
        } else {
            info!("No session {} for frame type {}", sid, ft);
        }
    }

    /// Feed raw link data and extract any complete frames.
    ///
    /// Called when `on_link_data` fires. The data may be a partial chunk of a
    /// larger frame, so we buffer and try to decode complete frames.
    pub async fn receive_data(&self, data: &[u8]) -> Vec<Frame> {
        let mut buf = self.inner.recv_buf.lock().await;
        buf.extend_from_slice(data);
        // info!("buf: {:?}", buf);
        let mut frames = Vec::new();
        loop {
            match Frame::decode(&buf) {
                Ok((frame, consumed)) => {
                    // info!("consume {}",consumed);
                    buf.drain(..consumed);
                    frames.push(frame);
                }
                Err(err) => {
                    match err {
                        // Finished => {
                        //     // finished decoding all packets basically
                        //     // though there might still be like 5 bytes left in the buffer
                        //     info!("finished");
                        //     break;
                        // }
                        MoreDataRequired => {
                            // info!("more data required");
                            break;
                            // just wait for next packet
                        }
                        DecodingFailed => {
                            // A damaged or out-of-sync frame must not crash the
                            // event-loop worker. Discard the buffered stream and
                            // wait for the next frame boundary.
                            error!(
                                "frame decode failed; dropping {} buffered bytes ({} new bytes)",
                                buf.len(),
                                data.len()
                            );
                            error!("buffer prefix: {:?}", &buf[..buf.len().min(16)]);
                            buf.clear();
                            break;
                        }
                    }
                }
            }
        }

        // // info!("frames {:?}", &frames);
        // if buf.len() > 12000 {
        //     warn!("buf thing: {:?}", &buf);
        //     // assert!(false);
        // }
        frames
    }
}

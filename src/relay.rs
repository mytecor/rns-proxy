//! Bidirectional TCP ↔ RNS relay — used by both client and server sessions.

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
    let tcp_to_rns = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    mux_fwd.send(FrameType::Data, sid, buf[..n].to_vec());
                }
                Err(e) => {
                    debug!("[{}] TCP read error: {}", sid, e);
                    break;
                }
            }
        }
    });

    // RNS -> TCP
    let rns_to_tcp = tokio::spawn(async move {
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
        _ = tcp_to_rns => {},
        _ = rns_to_tcp => {},
    }

    mux.send(FrameType::Close, sid, Vec::new());
    mux.drop_session(sid);
}

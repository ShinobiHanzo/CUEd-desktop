//! What the edge and the client share: one control connection carrying many
//! streams, a writer task with keepalives, a reader with an idle timeout, and
//! the per-stream byte pump.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::frame::{read_frame, write_frame, Frame, FrameType, MAX_PAYLOAD};
use crate::status::FunnelStatus;
use crate::{IDLE_TIMEOUT, PING_INTERVAL};

/// Frames queued for the writer before backpressure reaches the streams.
const OUTBOUND_QUEUE: usize = 256;
/// Chunks queued per stream before backpressure reaches the control reader.
const STREAM_QUEUE: usize = 64;

/// One control connection's shared state.
pub(crate) struct Mux {
    tx: mpsc::Sender<Frame>,
    streams: Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>,
    next_id: AtomicU32,
    /// Cancelled when the control connection is over, for every task hanging off it.
    pub cancel: CancellationToken,
}

impl Mux {
    pub fn new(cancel: CancellationToken) -> (Arc<Mux>, mpsc::Receiver<Frame>) {
        let (tx, rx) = mpsc::channel(OUTBOUND_QUEUE);
        (Arc::new(Mux { tx, streams: Mutex::new(HashMap::new()), next_id: AtomicU32::new(1), cancel }), rx)
    }

    fn streams(&self) -> std::sync::MutexGuard<'_, HashMap<u32, mpsc::Sender<Vec<u8>>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a frame for the peer; `false` once the connection is gone.
    pub async fn send(&self, frame: Frame) -> bool {
        self.tx.send(frame).await.is_ok()
    }

    /// Edge only: the next stream id to open.
    pub fn next_stream_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Creates the inbound channel for a stream. Bytes the peer sends for it
    /// land in the returned receiver.
    pub fn register(&self, id: u32) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        self.streams().insert(id, tx);
        rx
    }

    /// Forgets a stream; its pump sees the inbound channel close.
    pub fn unregister(&self, id: u32) -> bool {
        self.streams().remove(&id).is_some()
    }

    pub fn sender(&self, id: u32) -> Option<mpsc::Sender<Vec<u8>>> {
        self.streams().get(&id).cloned()
    }

    pub fn close_all(&self) {
        self.streams().clear();
    }
}

/// Writes queued frames and a `PING` every [`PING_INTERVAL`] until cancelled
/// or the queue closes.
pub(crate) async fn write_loop<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::Receiver<Frame>, cancel: CancellationToken) -> io::Result<()> {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            frame = rx.recv() => match frame {
                Some(f) => if let Err(e) = write_frame(&mut w, &f).await { break Err(e) },
                None => break Ok(()),
            },
            _ = ping.tick() => if let Err(e) = write_frame(&mut w, &Frame::ping()).await { break Err(e) },
        }
    };
    let _ = w.shutdown().await;
    result
}

/// Reads frames until the peer goes away, stays silent for [`IDLE_TIMEOUT`]
/// or the mux is cancelled. `DATA`, `CLOSE`, `PING` and `PONG` are handled
/// here; `OPEN` goes to `on_open(stream_id, remote_addr)`.
pub(crate) async fn read_loop<R, F>(mut r: R, mux: Arc<Mux>, mut on_open: F) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(u32, String),
{
    loop {
        let frame = tokio::select! {
            _ = mux.cancel.cancelled() => return Ok(()),
            res = tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut r)) => match res {
                Ok(Ok(frame)) => frame,
                Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, format!("peer silent for {IDLE_TIMEOUT:?}"))),
            },
        };
        match frame.kind {
            FrameType::Open => on_open(frame.stream, String::from_utf8_lossy(&frame.payload).into_owned()),
            FrameType::Data => match mux.sender(frame.stream) {
                Some(tx) => {
                    if tx.send(frame.payload).await.is_err() {
                        mux.unregister(frame.stream);
                        mux.send(Frame::close(frame.stream)).await;
                    }
                }
                None => {
                    // The stream is already gone on this side; tell the peer once.
                    mux.send(Frame::close(frame.stream)).await;
                }
            },
            FrameType::Close => {
                mux.unregister(frame.stream);
            }
            FrameType::Ping => {
                mux.send(Frame::pong()).await;
            }
            FrameType::Pong => {}
        }
    }
}

/// Moves bytes between one socket and one stream until either side ends.
/// Bytes read from the socket go out as `DATA`; bytes from `from_peer` are
/// written to the socket. On exit the stream is unregistered and, when this
/// side ended it, a `CLOSE` is sent.
pub(crate) async fn pump<S>(sock: S, id: u32, mux: Arc<Mux>, mut from_peer: mpsc::Receiver<Vec<u8>>, status: Option<Arc<FunnelStatus>>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut rd, mut wr) = tokio::io::split(sock);
    let mut buf = vec![0u8; MAX_PAYLOAD];
    let mut send_close = false;
    loop {
        tokio::select! {
            _ = mux.cancel.cancelled() => break,
            n = rd.read(&mut buf) => match n {
                Ok(0) | Err(_) => { send_close = true; break; }
                Ok(n) => {
                    if !mux.send(Frame::data(id, buf[..n].to_vec())).await { break; }
                    if let Some(s) = &status { s.bytes_out.fetch_add(n as u64, Ordering::Relaxed); }
                }
            },
            chunk = from_peer.recv() => match chunk {
                None => break, // peer closed the stream (or the connection ended)
                Some(bytes) => {
                    if wr.write_all(&bytes).await.is_err() { send_close = true; break; }
                    if let Some(s) = &status { s.bytes_in.fetch_add(bytes.len() as u64, Ordering::Relaxed); }
                }
            },
        }
    }
    mux.unregister(id);
    if send_close {
        mux.send(Frame::close(id)).await;
    }
    let _ = wr.shutdown().await;
}

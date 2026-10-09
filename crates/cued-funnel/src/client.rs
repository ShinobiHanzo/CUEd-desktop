//! The daemon's half: dials the edge, authenticates, and serves every stream
//! the edge opens by connecting to the local TLS listener.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::frame::{handshake_client, Frame};
use crate::mux::{pump, read_loop, write_loop, Mux};
use crate::status::FunnelStatus;
use crate::HANDSHAKE_TIMEOUT;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The edge's control endpoint, `host:port`.
    pub control: String,
    /// Pre-shared key, raw bytes.
    pub psk: Vec<u8>,
    /// Where opened streams are connected to: the daemon's TLS listener.
    pub forward_to: SocketAddr,
}

/// Shortest and longest pause between reconnect attempts.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Keeps a control connection to the edge until `shutdown` is cancelled,
/// reconnecting with exponential backoff (1 s … 60 s). `status` reflects the
/// live state for the daemon's UI.
pub async fn run_client(cfg: ClientConfig, status: Arc<FunnelStatus>, shutdown: CancellationToken) -> anyhow::Result<()> {
    anyhow::ensure!(!cfg.psk.is_empty(), "the pre-shared key must not be empty");
    let mut backoff = BACKOFF_MIN;
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let attempt = tokio::select! {
            _ = shutdown.cancelled() => break,
            r = connect(&cfg) => r,
        };
        match attempt {
            Ok(stream) => {
                tracing::info!(control = %cfg.control, "funnel connected");
                status.set_connected(true);
                status.clear_error();
                backoff = BACKOFF_MIN;
                let result = session(stream, &cfg, &status, &shutdown).await;
                status.set_connected(false);
                match result {
                    Ok(()) => tracing::info!("funnel control connection closed"),
                    Err(e) => {
                        tracing::warn!("funnel control connection failed: {e}");
                        status.set_error(e.to_string());
                    }
                }
            }
            Err(e) => {
                tracing::warn!(control = %cfg.control, "funnel connect failed: {e}; retrying in {backoff:?}");
                status.set_error(e.to_string());
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    status.set_connected(false);
    Ok(())
}

async fn connect(cfg: &ClientConfig) -> std::io::Result<TcpStream> {
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&cfg.control))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;
    let _ = stream.set_nodelay(true);
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake_client(&mut stream, &cfg.psk))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timed out"))??;
    Ok(stream)
}

async fn session(stream: TcpStream, cfg: &ClientConfig, status: &Arc<FunnelStatus>, shutdown: &CancellationToken) -> std::io::Result<()> {
    let cancel = shutdown.child_token();
    let (mux, rx) = Mux::new(cancel.clone());
    let (rd, wr) = stream.into_split();
    let writer = tokio::spawn(write_loop(wr, rx, cancel.clone()));
    let forward_to = cfg.forward_to;
    let result = read_loop(rd, Arc::clone(&mux), |id, remote| {
        let from_edge = mux.register(id);
        status.streams_total.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(serve_stream(id, remote, from_edge, Arc::clone(&mux), forward_to, Arc::clone(status)));
    })
    .await;
    cancel.cancel();
    mux.close_all();
    let _ = writer.await;
    result
}

async fn serve_stream(id: u32, remote: String, from_edge: tokio::sync::mpsc::Receiver<Vec<u8>>, mux: Arc<Mux>, forward_to: SocketAddr, status: Arc<FunnelStatus>) {
    status.streams_open.fetch_add(1, Ordering::Relaxed);
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(forward_to)).await {
        Ok(Ok(local)) => {
            let _ = local.set_nodelay(true);
            tracing::debug!(stream = id, %remote, "stream open");
            pump(local, id, Arc::clone(&mux), from_edge, Some(Arc::clone(&status))).await;
            tracing::debug!(stream = id, %remote, "stream closed");
        }
        Ok(Err(e)) => {
            tracing::warn!(stream = id, %remote, "cannot reach {forward_to}: {e}");
            status.set_error(format!("cannot reach {forward_to}: {e}"));
            mux.unregister(id);
            mux.send(Frame::close(id)).await;
        }
        Err(_) => {
            tracing::warn!(stream = id, %remote, "connecting to {forward_to} timed out");
            mux.unregister(id);
            mux.send(Frame::close(id)).await;
        }
    }
    status.streams_open.fetch_sub(1, Ordering::Relaxed);
}

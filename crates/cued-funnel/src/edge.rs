//! The public half: accepts TCP on a public port and forwards each connection
//! as a stream over the control connection the daemon holds open.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::frame::{handshake_edge, Frame};
use crate::mux::{pump, read_loop, write_loop, Mux};
use crate::HANDSHAKE_TIMEOUT;

#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// Public port phones connect to (e.g. `0.0.0.0:443`).
    pub listen: SocketAddr,
    /// Port the daemon dials (e.g. `0.0.0.0:8771`).
    pub control: SocketAddr,
    /// Pre-shared key, raw bytes (the hex from `cued-funnel keygen`, decoded).
    pub psk: Vec<u8>,
}

type Slot = Arc<Mutex<Option<Arc<Mux>>>>;

/// An edge with its two listeners bound. `bind` first so port errors surface
/// before anything runs and so tests can learn ephemeral ports.
pub struct Edge {
    public: TcpListener,
    control: TcpListener,
    psk: Arc<Vec<u8>>,
}

/// Binds and runs the edge until `shutdown` is cancelled.
pub async fn run_edge(cfg: EdgeConfig, shutdown: CancellationToken) -> anyhow::Result<()> {
    Edge::bind(cfg).await?.run(shutdown).await
}

impl Edge {
    pub async fn bind(cfg: EdgeConfig) -> anyhow::Result<Edge> {
        anyhow::ensure!(!cfg.psk.is_empty(), "the pre-shared key must not be empty");
        let public = TcpListener::bind(cfg.listen).await.with_context(|| format!("binding public port {}", cfg.listen))?;
        let control = TcpListener::bind(cfg.control).await.with_context(|| format!("binding control port {}", cfg.control))?;
        Ok(Edge { public, control, psk: Arc::new(cfg.psk) })
    }

    /// Where phones connect.
    pub fn listen_addr(&self) -> SocketAddr {
        self.public.local_addr().expect("bound listener has an address")
    }

    /// Where the daemon connects.
    pub fn control_addr(&self) -> SocketAddr {
        self.control.local_addr().expect("bound listener has an address")
    }

    pub async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let slot: Slot = Arc::new(Mutex::new(None));
        tracing::info!(public = %self.listen_addr(), control = %self.control_addr(), "funnel edge listening");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                accepted = self.control.accept() => match accepted {
                    Ok((stream, peer)) => {
                        tokio::spawn(attach_client(stream, peer, Arc::clone(&self.psk), Arc::clone(&slot), shutdown.clone()));
                    }
                    Err(e) => tracing::warn!("control accept failed: {e}"),
                },
                accepted = self.public.accept() => match accepted {
                    Ok((stream, peer)) => {
                        tokio::spawn(serve_public(stream, peer, Arc::clone(&slot)));
                    }
                    Err(e) => tracing::warn!("public accept failed: {e}"),
                },
            }
        }
        if let Some(mux) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            mux.cancel.cancel();
        }
        Ok(())
    }
}

async fn attach_client(mut stream: TcpStream, peer: SocketAddr, psk: Arc<Vec<u8>>, slot: Slot, shutdown: CancellationToken) {
    let _ = stream.set_nodelay(true);
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake_edge(&mut stream, &psk)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(%peer, "control handshake refused: {e}");
            return; // dropping the socket closes it
        }
        Err(_) => {
            tracing::warn!(%peer, "control handshake timed out");
            return;
        }
    }
    let cancel = shutdown.child_token();
    let (mux, rx) = Mux::new(cancel.clone());
    let previous = slot.lock().unwrap_or_else(|e| e.into_inner()).replace(Arc::clone(&mux));
    if let Some(old) = previous {
        tracing::info!(%peer, "new client attached; replacing the previous one");
        old.cancel.cancel();
    } else {
        tracing::info!(%peer, "client attached");
    }

    let (rd, wr) = stream.into_split();
    let writer = tokio::spawn(write_loop(wr, rx, cancel.clone()));
    let result = read_loop(rd, Arc::clone(&mux), |id, _| {
        tracing::warn!(stream = id, "client tried to open a stream; only the edge opens streams");
    })
    .await;
    match result {
        Ok(()) => tracing::info!(%peer, "client detached"),
        Err(e) => tracing::warn!(%peer, "control connection failed: {e}"),
    }
    cancel.cancel();
    mux.close_all();
    {
        let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
        if s.as_ref().is_some_and(|m| Arc::ptr_eq(m, &mux)) {
            *s = None;
        }
    }
    let _ = writer.await;
}

async fn serve_public(stream: TcpStream, peer: SocketAddr, slot: Slot) {
    let Some(mux) = slot.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        tracing::debug!(%peer, "no client attached; refusing public connection");
        return; // immediate close
    };
    let _ = stream.set_nodelay(true);
    let id = mux.next_stream_id();
    let rx = mux.register(id);
    if !mux.send(Frame::open(id, &peer.to_string())).await {
        return;
    }
    tracing::debug!(%peer, stream = id, "stream opened");
    pump(stream, id, mux, rx, None).await;
    tracing::debug!(%peer, stream = id, "stream closed");
}

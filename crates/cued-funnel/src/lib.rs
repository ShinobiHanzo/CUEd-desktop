//! The CUEd self-hosted funnel (`docs/protocol.md` §5).
//!
//! ```text
//! phone ──TLS──▶ edge :443 ──mux over one TCP control connection──▶ cuedd :8771 (TLS listener)
//! ```
//!
//! - [`run_edge`] is the public half: it accepts TCP on a public port and
//!   forwards every connection as a stream over the single control connection
//!   the daemon keeps open to it. Only one client is attached at a time.
//! - [`run_client`] runs inside `cuedd`: it dials the edge, authenticates with
//!   the pre-shared key, and for every stream the edge opens connects to
//!   `forward_to` (the daemon's TLS listener) and pumps bytes both ways.
//! - [`tls`] has the self-signed certificate the daemon terminates TLS with
//!   ([`tls::load_or_create_self_signed`]), the listener that decrypts and
//!   forwards to the plain HTTP port ([`tls::serve_tls_forwarder`]) and the
//!   fingerprint-pinning verifier phones use ([`tls::PinnedFingerprintVerifier`]).
//!
//! The edge only ever sees ciphertext.

pub mod client;
pub mod edge;
pub mod frame;
mod mux;
pub mod status;
pub mod tls;

pub use client::{run_client, ClientConfig};
pub use edge::{run_edge, Edge, EdgeConfig};
pub use status::{FunnelStatus, FunnelStatusSnapshot};
pub use tokio_util::sync::CancellationToken;

/// Interval between keepalive `PING`s on the control connection.
pub const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);
/// A control connection silent for this long is dropped.
pub const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Time allowed for the PSK handshake.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

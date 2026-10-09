//! An embedded NIP-01 Nostr relay for the CUEd desktop (`docs/protocol.md` §4).
//!
//! The relay is transport-agnostic: the daemon mounts it on a websocket route
//! and drives one [`Connection`] per socket. Inbound text frames go through
//! [`Connection::handle`], which returns the replies to send back right away
//! (`OK`, stored `EVENT`s, `EOSE`, `CLOSED`, `NOTICE`); live `EVENT` pushes for
//! the connection's subscriptions come out of [`Connection::next_push`].
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! let relay = cued_relay::Relay::open(None)?; // in-memory
//! let mut conn = relay.connect();
//! // socket loop (pseudo-code):
//! // loop { tokio::select! {
//! //     Some(text) = socket.next_text() => for reply in conn.handle(&text) { socket.send(reply).await?; }
//! //     Some(push) = conn.next_push() => socket.send(push).await?,
//! // } }
//! # Ok(()) }
//! ```

mod filter;
mod relay;
mod store;

pub use cued_proto::Event;
pub use filter::Filter;
pub use relay::{Accept, Connection, Relay, RelayStats};

/// Largest event (serialised JSON) the relay accepts.
pub const MAX_EVENT_BYTES: usize = 64 * 1024;
/// Events dated further than this into the future are refused.
pub const MAX_FUTURE_SECS: i64 = 15 * 60;
/// `limit` applied when a filter has none.
pub const DEFAULT_LIMIT: usize = 500;
/// Largest `limit` a filter may ask for.
pub const MAX_LIMIT: usize = 1000;
/// Open subscriptions one connection may hold.
pub const MAX_SUBSCRIPTIONS: usize = 20;
/// Longest subscription id accepted.
pub const MAX_SUB_ID_LEN: usize = 64;

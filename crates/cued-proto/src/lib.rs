//! The protocol surface shared by every CUEd desktop component.
//!
//! Everything here mirrors `core/` in the phone repository (ShinobiHanzo/CUEd)
//! byte for byte where it matters: the `cued://` link grammar, the NIP-01
//! canonical form that is hashed into an event id, the BIP-340 signature over
//! that id, and the bech32 `npub`/`nsec` spelling of keys. The tests run the
//! same BIP-340 vectors the phone runs.

pub mod account;
pub mod bio;
pub mod friend;
pub mod link;
pub mod nip19;
pub mod nostr;
pub mod pair;
pub mod seal;
pub mod share;
pub mod station;

pub use account::{Block, Chain};
pub use friend::FriendLink;
pub use link::{decode_link, Link};
pub use nostr::{Event, Identity, RelayMessage};
pub use pair::{AccountBundle, PairLink, PairRequest, PairResponse, SealedAccount};
pub use share::SharePayload;
pub use station::{StationLink, StationState, StationTrack};

/// Scheme every CUEd link starts with.
pub const SCHEME: &str = "cued";
/// The download page; a share encoded as `<base>#<query>` opens the page on
/// phones without CUEd and the app on phones with it.
pub const SHARE_WEB_BASE: &str = "https://shinobihanzo.github.io/CUEd/";
pub const SHARE_PAYLOAD_VERSION: u32 = 1;
pub const PAIR_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a {0} link")]
    NotALink(&'static str),
    #[error("missing field `{0}`")]
    Missing(&'static str),
    #[error("bad field `{0}`")]
    Bad(&'static str),
    #[error("bad hex")]
    Hex(#[from] hex::FromHexError),
    #[error("bad key: {0}")]
    Key(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("chain: {0}")]
    Chain(String),
    #[error("seal: {0}")]
    Seal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

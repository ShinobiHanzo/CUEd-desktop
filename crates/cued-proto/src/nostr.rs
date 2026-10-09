//! The slice of Nostr (NIP-01) a station needs: signed events, canonical ids,
//! relay messages. Mirrors `core/station/Nostr.kt`; the canonical JSON form is
//! `[0, pubkey, created_at, kind, tags, content]` with no whitespace, which is
//! what both `kotlinx.serialization` and `serde_json` produce.

use k256::schnorr::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl Event {
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags.iter().find(|t| t.len() >= 2 && t[0] == name).map(|t| t[1].as_str())
    }

    /// Replaceable (kinds 0, 3, 10000..19999): a relay keeps one per author.
    pub fn is_replaceable(&self) -> bool {
        self.kind == 0 || self.kind == 3 || (10_000..20_000).contains(&self.kind)
    }

    /// Parameterised-replaceable (30000..39999): one per author and `d` tag.
    pub fn is_param_replaceable(&self) -> bool {
        (30_000..40_000).contains(&self.kind)
    }

    /// Ephemeral (20000..29999): never stored.
    pub fn is_ephemeral(&self) -> bool {
        (20_000..30_000).contains(&self.kind)
    }

    /// Recomputes the id and checks the BIP-340 signature.
    pub fn verify(&self) -> bool {
        let expected = id(&self.pubkey, self.created_at, self.kind, &self.tags, &self.content);
        if expected != self.id {
            return false;
        }
        verify_schnorr(&self.id, &self.pubkey, &self.sig)
    }
}

pub fn canonical(pubkey: &str, created_at: i64, kind: u32, tags: &[Vec<String>], content: &str) -> String {
    serde_json::to_string(&json!([0, pubkey, created_at, kind, tags, content])).expect("canonical json")
}

pub fn id(pubkey: &str, created_at: i64, kind: u32, tags: &[Vec<String>], content: &str) -> String {
    hex::encode(Sha256::digest(canonical(pubkey, created_at, kind, tags, content).as_bytes()))
}

pub fn verify_schnorr(msg_hex: &str, pubkey_hex: &str, sig_hex: &str) -> bool {
    let (Ok(msg), Ok(pk), Ok(sig)) = (hex::decode(msg_hex), hex::decode(pubkey_hex), hex::decode(sig_hex)) else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    let Ok(sig) = Signature::try_from(sig.as_slice()) else { return false };
    vk.verify_prehash(&msg, &sig).is_ok()
}

/// A secp256k1 key pair: the thing a station (phone or desktop) *is*.
#[derive(Clone)]
pub struct Identity {
    key: SigningKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.pubkey_hex())
    }
}

impl Identity {
    pub fn generate() -> Self {
        Self { key: SigningKey::random(&mut rand::thread_rng()) }
    }

    pub fn from_secret_hex(hex_sk: &str) -> Result<Self> {
        let bytes = hex::decode(hex_sk.trim())?;
        let key = SigningKey::from_bytes(&bytes).map_err(|e| Error::Key(e.to_string()))?;
        Ok(Self { key })
    }

    /// Accepts `nsec1…` or 64 hex characters.
    pub fn from_secret(text: &str) -> Result<Self> {
        match crate::nip19::parse(text) {
            Some(("nsec", hex)) | Some(("hex", hex)) => Self::from_secret_hex(&hex),
            Some(("npub", _)) => Err(Error::Key("that is a public key (npub); the secret key starts with nsec".into())),
            _ => Err(Error::Key("not an nsec or hex key".into())),
        }
    }

    pub fn secret_hex(&self) -> String {
        hex::encode(self.key.to_bytes())
    }

    /// x-only public key, 32 bytes, hex.
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.key.verifying_key().to_bytes())
    }

    pub fn npub(&self) -> String {
        crate::nip19::npub(&self.pubkey_hex())
    }

    pub fn nsec(&self) -> String {
        crate::nip19::nsec(&self.secret_hex())
    }

    /// BIP-340 signature over a 32-byte message with explicit aux randomness (test vectors).
    pub fn sign_raw(&self, msg: &[u8], aux: &[u8; 32]) -> [u8; 64] {
        let sig = self.key.sign_raw(msg, aux).expect("sign");
        sig.to_bytes()
    }

    pub fn sign(&self, kind: u32, tags: Vec<Vec<String>>, content: String, created_at: i64) -> Event {
        let pubkey = self.pubkey_hex();
        let id = id(&pubkey, created_at, kind, &tags, &content);
        let digest = hex::decode(&id).expect("id is hex");
        let sig: Signature = self.key.sign_prehash(&digest).expect("sign");
        Event { id, pubkey, created_at, kind, tags, content, sig: hex::encode(sig.to_bytes()) }
    }

    pub fn sign_now(&self, kind: u32, tags: Vec<Vec<String>>, content: String) -> Event {
        self.sign(kind, tags, content, now_secs())
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// What a relay can send a client.
#[derive(Debug, Clone, PartialEq)]
pub enum RelayMessage {
    Event { sub_id: String, event: Event },
    Eose(String),
    Ok { event_id: String, accepted: bool, message: String },
    Closed { sub_id: String, message: String },
    Notice(String),
    Unknown(String),
}

/// What a client can send a relay.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Event(Event),
    Req { sub_id: String, filters: Vec<Value> },
    Close(String),
    Unknown(String),
}

pub fn event_message(e: &Event) -> String {
    serde_json::to_string(&json!(["EVENT", e])).expect("json")
}
pub fn req_message(sub_id: &str, filters: &[Value]) -> String {
    let mut arr = vec![json!("REQ"), json!(sub_id)];
    arr.extend(filters.iter().cloned());
    serde_json::to_string(&Value::Array(arr)).expect("json")
}
pub fn close_message(sub_id: &str) -> String {
    serde_json::to_string(&json!(["CLOSE", sub_id])).expect("json")
}
pub fn ok_message(event_id: &str, accepted: bool, message: &str) -> String {
    serde_json::to_string(&json!(["OK", event_id, accepted, message])).expect("json")
}
pub fn eose_message(sub_id: &str) -> String {
    serde_json::to_string(&json!(["EOSE", sub_id])).expect("json")
}
pub fn closed_message(sub_id: &str, message: &str) -> String {
    serde_json::to_string(&json!(["CLOSED", sub_id, message])).expect("json")
}
pub fn notice_message(message: &str) -> String {
    serde_json::to_string(&json!(["NOTICE", message])).expect("json")
}
pub fn relay_event_message(sub_id: &str, e: &Event) -> String {
    serde_json::to_string(&json!(["EVENT", sub_id, e])).expect("json")
}

fn str_at(arr: &[Value], i: usize) -> Option<String> {
    arr.get(i).and_then(Value::as_str).map(str::to_string)
}

/// Parses a message *from* a relay.
pub fn parse_relay_message(text: &str) -> RelayMessage {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else { return RelayMessage::Unknown(text.into()) };
    match (str_at(&arr, 0).as_deref(), arr.len()) {
        (Some("EVENT"), n) if n >= 3 => match (str_at(&arr, 1), serde_json::from_value::<Event>(arr[2].clone())) {
            (Some(sub_id), Ok(event)) => RelayMessage::Event { sub_id, event },
            _ => RelayMessage::Unknown(text.into()),
        },
        (Some("EOSE"), n) if n >= 2 => str_at(&arr, 1).map(RelayMessage::Eose).unwrap_or(RelayMessage::Unknown(text.into())),
        (Some("OK"), n) if n >= 3 => RelayMessage::Ok {
            event_id: str_at(&arr, 1).unwrap_or_default(),
            accepted: arr[2].as_bool().unwrap_or(false),
            message: str_at(&arr, 3).unwrap_or_default(),
        },
        (Some("CLOSED"), n) if n >= 2 => RelayMessage::Closed { sub_id: str_at(&arr, 1).unwrap_or_default(), message: str_at(&arr, 2).unwrap_or_default() },
        (Some("NOTICE"), _) => RelayMessage::Notice(str_at(&arr, 1).unwrap_or_default()),
        _ => RelayMessage::Unknown(text.into()),
    }
}

/// Parses a message *from* a client (what a relay receives).
pub fn parse_client_message(text: &str) -> ClientMessage {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else { return ClientMessage::Unknown(text.into()) };
    match (str_at(&arr, 0).as_deref(), arr.len()) {
        (Some("EVENT"), n) if n >= 2 => serde_json::from_value::<Event>(arr[1].clone()).map(ClientMessage::Event).unwrap_or(ClientMessage::Unknown(text.into())),
        (Some("REQ"), n) if n >= 2 => match str_at(&arr, 1) {
            Some(sub_id) => ClientMessage::Req { sub_id, filters: arr[2..].iter().filter(|f| f.is_object()).cloned().collect() },
            None => ClientMessage::Unknown(text.into()),
        },
        (Some("CLOSE"), n) if n >= 2 => str_at(&arr, 1).map(ClientMessage::Close).unwrap_or(ClientMessage::Unknown(text.into())),
        _ => ClientMessage::Unknown(text.into()),
    }
}

/// Filter for the latest station state of these authors.
pub fn station_filter(authors: &[String], since: Option<i64>) -> Value {
    let mut f = json!({"kinds": [crate::station::KIND], "authors": authors, "#d": [crate::station::D_TAG]});
    if let Some(s) = since {
        f["since"] = json!(s);
    }
    f
}

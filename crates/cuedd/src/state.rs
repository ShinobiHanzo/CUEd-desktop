//! Everything the request handlers share.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use cued_proto::Identity;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::account::AccountStore;
use crate::config::Settings;
use crate::db::Db;
use crate::player::Player;
use crate::store::Store;

/// Which listener a request came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The LAN port: the browser on this machine (admin when loopback) and phones on the Wi-Fi.
    Lan,
    /// The funnel's TLS forwarder: never admin, bearer only.
    External,
}

pub struct Options {
    pub data_dir: PathBuf,
    pub port: u16,
    pub name: Option<String>,
    pub no_audio: bool,
    pub funnel_public: Option<String>,
    pub funnel_enabled: bool,
}

/// A pairing secret waiting for a phone.
pub struct PendingPair {
    pub secret: String,
    pub created: Instant,
}

/// A time-limited capability for `/share/<sha>/<cap>`.
pub struct ShareCap {
    pub sha256: String,
    pub expires: Instant,
}

/// A biometric challenge handed to a phone (`docs/protocol.md` §7).
pub struct Challenge {
    pub device: String,
    pub issued: Instant,
}

/// Which tier a request was admitted at. Set by the policy middleware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Admin,
    Lan,
    Remote,
}

/// Something the UI wants to hear about live.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UiEvent {
    DevicePaired { id: String, name: String },
    DeviceRemoved { id: String },
    TrackAdded { sha256: String, title: String, artist: String, source: String },
    Upload { sha256: String, have: u64, total: u64 },
    Station { pubkey: String },
    Relay { connections: usize },
    Companion { job: String, status: String, message: String },
    Friends,
    Shared,
    Account,
    Listen,
    /// A party guest did something to the queue.
    Party { who: String, what: String },
    #[allow(dead_code)]
    Notice { text: String },
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub port: u16,
    pub db: Db,
    pub store: Store,
    pub settings: Mutex<Settings>,
    pub account: AccountStore,
    pub relay: Arc<cued_relay::Relay>,
    pub player: Player,
    pub pending_pairs: Mutex<Vec<PendingPair>>,
    /// Secrets of phones paired in the last 10 minutes, so a `keep` import can be opened.
    pub recent_pair_secrets: Mutex<HashMap<String, (String, Instant)>>,
    pub share_caps: Mutex<HashMap<String, ShareCap>>,
    pub challenges: Mutex<HashMap<String, Challenge>>,
    /// Party request nonces seen lately (replay protection).
    pub party_nonces: Mutex<crate::party::NonceCache>,
    /// Verified assertions: `<challenge>.<sig>` → (device, verified at).
    pub assertions: Mutex<HashMap<String, (String, Instant)>>,
    pub listen: crate::listen::ListenShared,
    pub events: broadcast::Sender<UiEvent>,
    pub http: reqwest::Client,
    pub funnel_status: Arc<cued_funnel::FunnelStatus>,
    pub funnel_public: Option<String>,
    pub tls_config: Option<Arc<rustls::ServerConfig>>,
    pub tls_fingerprint: Option<String>,
    pub companion: Mutex<Option<crate::companion::CompanionInfo>>,
    pub stations: crate::stations::StationsShared,
    pub started: Instant,
}

impl AppState {
    pub async fn open(o: Options) -> anyhow::Result<Arc<Self>> {
        let settings_path = o.data_dir.join("settings.json");
        let mut settings = Settings::load(&settings_path);
        if let Some(n) = o.name {
            settings.name = n;
        }
        settings.save(&settings_path)?;
        let db = Db::open(&o.data_dir.join("cued.db"))?;
        let store = Store::new(&o.data_dir)?;
        let account = AccountStore::open(&o.data_dir, &settings.name).context("opening the account")?;
        info!("account {} ({}), chain height {}", account.identity().npub(), account.chain().name(), account.chain().height());
        let relay = cued_relay::Relay::open(Some(&o.data_dir.join("relay.db"))).context("opening relay store")?;
        let player = Player::new(!o.no_audio, settings.crossfade_seconds, settings.crossfade_curve);
        player.set_karaoke(settings.karaoke, settings.karaoke_strength);
        let (events, _) = broadcast::channel(256);

        let (tls_config, tls_fingerprint) = if o.funnel_enabled {
            match cued_funnel::tls::load_or_create_self_signed(&o.data_dir, &settings.name) {
                Ok((cfg, fp)) => (Some(cfg), Some(fp)),
                Err(e) => {
                    warn!("funnel tls: {e}");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        Ok(Arc::new(Self {
            data_dir: o.data_dir,
            port: o.port,
            db,
            store,
            settings: Mutex::new(settings),
            account,
            relay,
            player,
            pending_pairs: Mutex::new(Vec::new()),
            recent_pair_secrets: Mutex::new(HashMap::new()),
            share_caps: Mutex::new(HashMap::new()),
            challenges: Mutex::new(HashMap::new()),
            party_nonces: Mutex::new(Default::default()),
            assertions: Mutex::new(HashMap::new()),
            listen: Default::default(),
            events,
            http: reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(600)).build()?,
            funnel_status: Arc::new(cued_funnel::FunnelStatus::default()),
            funnel_public: o.funnel_public,
            tls_config,
            tls_fingerprint,
            companion: Mutex::new(None),
            stations: Default::default(),
            started: Instant::now(),
        }))
    }

    pub async fn close(&self) {
        self.player.stop();
    }

    /// The account's key pair (the thing that signs stations, friend events and chain blocks).
    pub fn identity(&self) -> Identity {
        self.account.identity()
    }

    pub fn relay_enabled(&self) -> bool {
        self.account.relay_enabled()
    }

    /// Base URL other people's devices should use for capability links: the funnel when there is one, else the LAN.
    pub fn public_base(&self) -> String {
        self.funnel_url().unwrap_or_else(|| self.lan_urls().into_iter().next().unwrap_or_else(|| format!("http://127.0.0.1:{}", self.port)))
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> anyhow::Result<Settings> {
        let mut s = self.settings.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut s);
        s.save(&self.data_dir.join("settings.json"))?;
        self.player.set_crossfade(s.crossfade_seconds, s.crossfade_curve);
        self.player.set_karaoke(s.karaoke, s.karaoke_strength);
        Ok(s.clone())
    }

    pub fn emit(&self, e: UiEvent) {
        let _ = self.events.send(e);
    }

    pub fn lan_urls(&self) -> Vec<String> {
        crate::net::advertised_hosts().into_iter().map(|h| format!("http://{h}:{}", self.port)).collect()
    }

    pub fn funnel_url(&self) -> Option<String> {
        self.funnel_public.as_ref().map(|h| format!("https://{h}"))
    }

    pub fn relay_url(&self) -> String {
        self.lan_urls().first().map(|u| format!("{}/relay", u.replacen("http", "ws", 1))).unwrap_or_else(|| format!("ws://127.0.0.1:{}/relay", self.port))
    }

    /// A fresh `cued://pair` link; the secret is single-use and expires after 10 minutes.
    pub fn new_pair_link(&self) -> cued_proto::PairLink {
        let secret = cued_proto::pair::new_secret();
        {
            let mut p = self.pending_pairs.lock().unwrap_or_else(|p| p.into_inner());
            p.retain(|x| x.created.elapsed() < PAIR_TTL);
            p.push(PendingPair { secret: secret.clone(), created: Instant::now() });
        }
        let settings = self.settings();
        cued_proto::PairLink {
            version: cued_proto::PAIR_VERSION,
            name: settings.name,
            addresses: self.lan_urls(),
            funnel: self.funnel_url(),
            cert_sha256: self.tls_fingerprint.clone(),
            secret,
            pubkey: Some(self.identity().pubkey_hex()),
            relay: Some(self.relay_url()),
        }
    }

    /// Finds the pending secret that signs `req`, consumes it and returns it.
    pub fn take_pair_secret(&self, req: &cued_proto::PairRequest) -> Option<String> {
        let mut p = self.pending_pairs.lock().unwrap_or_else(|p| p.into_inner());
        p.retain(|x| x.created.elapsed() < PAIR_TTL);
        let i = p.iter().position(|x| req.verify(&x.secret))?;
        let secret = p.remove(i).secret;
        let mut r = self.recent_pair_secrets.lock().unwrap_or_else(|p| p.into_inner());
        r.retain(|_, (_, t)| t.elapsed() < PAIR_TTL);
        r.insert(req.device.clone(), (secret.clone(), Instant::now()));
        Some(secret)
    }

    pub fn recent_pair_secret(&self, device: &str) -> Option<String> {
        let mut r = self.recent_pair_secrets.lock().unwrap_or_else(|p| p.into_inner());
        r.retain(|_, (_, t)| t.elapsed() < PAIR_TTL);
        r.get(device).map(|(s, _)| s.clone())
    }

    pub fn new_challenge(&self, device: &str) -> String {
        let c = cued_proto::bio::new_challenge();
        let mut m = self.challenges.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|_, ch| ch.issued.elapsed() < CHALLENGE_TTL);
        m.insert(c.clone(), Challenge { device: device.into(), issued: Instant::now() });
        c
    }

    /// `true` when `assertion` (`<challenge>.<sig>`) is a live biometric assertion by `device`.
    pub fn check_assertion(&self, device: &crate::db::Device, assertion: &str) -> bool {
        if device.biokey.is_empty() {
            return false;
        }
        {
            let mut a = self.assertions.lock().unwrap_or_else(|p| p.into_inner());
            a.retain(|_, (_, t)| t.elapsed() < ASSERTION_TTL);
            if let Some((d, _)) = a.get(assertion) {
                return d == &device.id;
            }
        }
        let Some((challenge, sig)) = assertion.split_once('.') else { return false };
        let live = {
            let mut m = self.challenges.lock().unwrap_or_else(|p| p.into_inner());
            m.retain(|_, ch| ch.issued.elapsed() < CHALLENGE_TTL);
            m.get(challenge).map(|ch| ch.device == device.id).unwrap_or(false)
        };
        if !live || !cued_proto::bio::verify(&device.biokey, &device.id, challenge, sig) {
            return false;
        }
        // One challenge, one assertion: the signed string is what lives on for ASSERTION_TTL.
        self.challenges.lock().unwrap_or_else(|p| p.into_inner()).remove(challenge);
        self.assertions.lock().unwrap_or_else(|p| p.into_inner()).insert(assertion.to_string(), (device.id.clone(), Instant::now()));
        true
    }

    pub fn pending_pair_count(&self) -> usize {
        let mut p = self.pending_pairs.lock().unwrap_or_else(|p| p.into_inner());
        p.retain(|x| x.created.elapsed() < PAIR_TTL);
        p.len()
    }

    pub fn new_share_cap(&self, sha256: &str) -> String {
        self.new_share_cap_for(sha256, SHARE_TTL)
    }

    pub fn new_share_cap_for(&self, sha256: &str, ttl: Duration) -> String {
        let cap = hex::encode(rand::random::<[u8; 12]>());
        let mut m = self.share_caps.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|_, c| c.expires > Instant::now());
        m.insert(cap.clone(), ShareCap { sha256: sha256.into(), expires: Instant::now() + ttl });
        cap
    }

    pub fn check_share_cap(&self, sha256: &str, cap: &str) -> bool {
        let m = self.share_caps.lock().unwrap_or_else(|p| p.into_inner());
        m.get(cap).map(|c| c.sha256 == sha256 && c.expires > Instant::now()).unwrap_or(false)
    }
}

const PAIR_TTL: Duration = Duration::from_secs(600);
const SHARE_TTL: Duration = Duration::from_secs(1800);
/// Challenges and the assertions signed over them are good for this long.
const ASSERTION_TTL: Duration = Duration::from_secs(600);
/// A challenge must be signed within this window (the phone's biometric prompt is on screen).
const CHALLENGE_TTL: Duration = Duration::from_secs(120);

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn new_token() -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

pub fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// "Same LAN directly": loopback or an RFC 1918 address.
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => a.is_loopback() || a.to_ipv4_mapped().map(|v4| v4.is_private() || v4.is_loopback()).unwrap_or(false) || (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}

//! `cuedd`: the CUEd desktop daemon. One process, one port, a web UI, a
//! phone-facing API, an embedded Nostr relay, a crossfading player and an
//! optional self-hosted funnel. See `docs/protocol.md`.

mod api;
mod companion;
mod config;
mod db;
mod identity;
mod net;
mod player;
mod probe;
mod relay_ws;
mod state;
mod stations;
mod store;
mod ui;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::state::{AppState, Origin};

#[derive(Parser, Debug)]
#[command(name = "cuedd", version, about = "CUEd desktop daemon")]
struct Args {
    /// Where the library, backups, database and keys live.
    #[arg(long, env = "CUED_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Port for the UI, the phone API and the relay.
    #[arg(long, env = "CUED_PORT", default_value_t = 8770)]
    port: u16,
    /// Bind address for the LAN listener. Use 127.0.0.1 to keep it local.
    #[arg(long, env = "CUED_BIND", default_value = "0.0.0.0")]
    bind: IpAddr,
    /// Name shown on the phone when pairing. Defaults to the hostname.
    #[arg(long, env = "CUED_NAME")]
    name: Option<String>,
    /// Funnel edge control address (host:port). Enables the funnel.
    #[arg(long, env = "CUED_FUNNEL")]
    funnel: Option<String>,
    /// Pre-shared key for the funnel edge (hex), from `cued-funnel keygen`.
    #[arg(long, env = "CUED_FUNNEL_PSK")]
    funnel_psk: Option<String>,
    /// Public address phones should use for the funnel (host:port), if different from the control host.
    #[arg(long, env = "CUED_FUNNEL_PUBLIC")]
    funnel_public: Option<String>,
    /// Port of the TLS listener the funnel forwards into (loopback only).
    #[arg(long, env = "CUED_TLS_PORT", default_value_t = 8771)]
    tls_port: u16,
    /// Do not start the Python download worker.
    #[arg(long)]
    no_companion: bool,
    /// Do not start the audio engine (headless box).
    #[arg(long, env = "CUED_NO_AUDIO")]
    no_audio: bool,
    /// Open the UI in the default browser once listening.
    #[arg(long)]
    open: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hyper=warn,rustls=warn".into()))
        .init();
    let args = Args::parse();

    let data_dir = args.data_dir.clone().unwrap_or_else(config::default_data_dir);
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    info!("data directory {}", data_dir.display());

    let shutdown = CancellationToken::new();
    let state = AppState::open(state::Options {
        data_dir: data_dir.clone(),
        port: args.port,
        name: args.name.clone(),
        no_audio: args.no_audio,
        funnel_public: args.funnel_public.clone().or_else(|| args.funnel.clone()),
        funnel_enabled: args.funnel.is_some(),
    })
    .await?;

    // LAN listener: the phone, the browser and the relay all come in here.
    let lan_addr = SocketAddr::new(args.bind, args.port);
    let lan = tokio::net::TcpListener::bind(lan_addr).await.with_context(|| format!("binding {lan_addr}"))?;
    info!("listening on http://{lan_addr}");
    for url in state.lan_urls() {
        info!("  phones on the LAN: {url}   relay: {}/relay", url.replacen("http", "ws", 1));
    }

    // External listener: what the funnel's TLS forwarder decrypts into. Loopback only, never trusted as admin.
    let ext_port = args.tls_port + 1;
    let ext_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ext_port);
    let ext = tokio::net::TcpListener::bind(ext_addr).await.with_context(|| format!("binding {ext_addr}"))?;

    let app_lan = api::router(state.clone(), Origin::Lan);
    let app_ext = api::router(state.clone(), Origin::External);

    // Background services.
    stations::spawn(state.clone(), shutdown.clone());
    if !args.no_companion {
        companion::spawn(state.clone(), shutdown.clone());
    }
    if let Some(control) = args.funnel.clone() {
        let psk = args.funnel_psk.clone().context("--funnel needs --funnel-psk (run `cued-funnel keygen`)")?;
        let psk = hex::decode(psk.trim()).context("funnel psk must be hex")?;
        spawn_funnel(state.clone(), control, psk, args.tls_port, ext_port, shutdown.clone()).await?;
    }

    if args.open {
        let url = format!("http://127.0.0.1:{}/", args.port);
        if let Err(e) = open::that(&url) {
            warn!("could not open a browser: {e}");
        }
    }

    let sd = shutdown.clone();
    let lan_srv = axum::serve(lan, app_lan.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async move { sd.cancelled().await });
    let sd = shutdown.clone();
    let ext_srv = axum::serve(ext, app_ext.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async move { sd.cancelled().await });

    tokio::select! {
        r = lan_srv => { if let Err(e) = r { error!("lan server: {e}"); } }
        r = ext_srv => { if let Err(e) = r { error!("external server: {e}"); } }
        _ = tokio::signal::ctrl_c() => { info!("shutting down"); }
    }
    shutdown.cancel();
    state.close().await;
    Ok(())
}

async fn spawn_funnel(state: Arc<AppState>, control: String, psk: Vec<u8>, tls_port: u16, ext_port: u16, shutdown: CancellationToken) -> anyhow::Result<()> {
    let tls_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tls_port);
    let ext_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ext_port);
    let config = state.tls_config.clone().context("tls config")?;
    let sd = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = cued_funnel::tls::serve_tls_forwarder(tls_addr, config, ext_addr, sd).await {
            error!("tls forwarder: {e}");
        }
    });
    let status = state.funnel_status.clone();
    let cfg = cued_funnel::ClientConfig { control: control.clone(), psk, forward_to: tls_addr };
    let sd = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = cued_funnel::run_client(cfg, status, sd).await {
            error!("funnel client: {e}");
        }
    });
    info!("funnel: connecting to edge {control}; phones use https://{} (cert {})", state.funnel_public.as_deref().unwrap_or("?"), state.tls_fingerprint.as_deref().unwrap_or("?"));
    Ok(())
}

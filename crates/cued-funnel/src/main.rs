//! `cued-funnel`: the edge binary (`docs/protocol.md` §5).

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use cued_funnel::{CancellationToken, EdgeConfig};

#[derive(Parser)]
#[command(name = "cued-funnel", version, about = "Self-hosted funnel edge for the CUEd desktop")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print a fresh random 32-byte pre-shared key as hex
    Keygen,
    /// Run the public edge: accept phones on --listen, accept cuedd on --control
    Edge {
        /// Public address phones connect to
        #[arg(long, default_value = "0.0.0.0:443")]
        listen: SocketAddr,
        /// Address cuedd dials from home
        #[arg(long, default_value = "0.0.0.0:8771")]
        control: SocketAddr,
        /// Pre-shared key (hex, from `cued-funnel keygen`); also read from CUED_FUNNEL_PSK
        #[arg(long, env = "CUED_FUNNEL_PSK", hide_env_values = true)]
        psk: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().cmd {
        Cmd::Keygen => {
            let key: [u8; 32] = rand::random();
            println!("{}", hex::encode(key));
            Ok(())
        }
        Cmd::Edge { listen, control, psk } => {
            let psk = hex::decode(psk.trim()).map_err(|e| anyhow::anyhow!("--psk must be hex: {e}"))?;
            anyhow::ensure!(psk.len() >= 16, "--psk is too short; use `cued-funnel keygen` to make one");
            let edge = cued_funnel::Edge::bind(EdgeConfig { listen, control, psk: psk.clone() }).await?;
            let public_port = edge.listen_addr().port();
            let control_port = edge.control_addr().port();
            eprintln!(
                "cued-funnel edge\n  phones connect to : https://<this host>:{public_port}  (listening on {listen})\n  cuedd connects to : <this host>:{control_port}         (listening on {control})\n\nIn cuedd, set\n  funnel control = <public hostname or IP of this box>:{control_port}\n  funnel psk     = {}\n\nThe edge only forwards ciphertext; TLS ends on the desktop, whose certificate the phone pins.\n",
                hex::encode(&psk)
            );
            let shutdown = CancellationToken::new();
            let stopper = shutdown.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    tracing::info!("shutting down");
                    stopper.cancel();
                }
            });
            edge.run(shutdown).await
        }
    }
}

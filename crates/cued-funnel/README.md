# cued-funnel

The self-hosted funnel from `docs/protocol.md` §5: one small edge binary on a
box with a public address, one client inside `cuedd`. The edge never sees
plaintext; TLS ends on the desktop with a self-signed certificate the phone
pins by fingerprint.

```
phone ──TLS──▶ edge :443 ──mux over one TCP control connection──▶ cuedd :8771 (TLS listener) ──▶ cuedd :8770 (HTTP)
```

## Edge (public box)

```sh
cued-funnel keygen                         # prints a 32-byte hex PSK
cued-funnel edge --listen 0.0.0.0:443 --control 0.0.0.0:8771 --psk <hex>
# or: CUED_FUNNEL_PSK=<hex> cued-funnel edge
```

The edge accepts the daemon on `--control` (one client at a time; a new one
replaces the old), accepts phones on `--listen`, and closes public connections
immediately while no daemon is attached. Logging via `RUST_LOG`.

## Daemon side (library)

```rust
use cued_funnel::{tls, run_client, ClientConfig, FunnelStatus, CancellationToken};

let (server_config, fingerprint) = tls::load_or_create_self_signed(&data_dir, &desktop_name)?; // `c` in the pair QR
tokio::spawn(tls::serve_tls_forwarder("0.0.0.0:8771".parse()?, server_config, "127.0.0.1:8770".parse()?, shutdown.clone()));
let status = Arc::new(FunnelStatus::new());
tokio::spawn(run_client(
    ClientConfig { control: "edge.example.com:8771".into(), psk, forward_to: "127.0.0.1:8771".parse()? },
    status.clone(),
    shutdown,
));
// status.snapshot() -> FunnelStatusSnapshot (serde) for the UI
```

`tls::PinnedFingerprintVerifier` is the client-side half of the pin: a rustls
`ServerCertVerifier` that accepts only the certificate with the given SHA-256,
with `PinnedFingerprintVerifier::connector(&fingerprint_hex)` for tooling and
simulators.

## Wire protocol

- Handshake: edge → 32 random bytes; client → `HMAC-SHA256(psk, nonce)`;
  edge → `0x01`, or closes.
- Frames: `type u8 | stream u32 BE | len u32 BE | payload`, payload ≤ 64 KiB.
  `1 OPEN` (payload: remote address), `2 DATA`, `3 CLOSE`, `4 PING`, `5 PONG`.
  Only the edge opens streams.
- Both sides `PING` every 20 s and drop the connection after 60 s of silence.
- The client reconnects with exponential backoff, 1 s to 60 s.

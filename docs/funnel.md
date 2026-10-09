# Self-hosted funnel

Carrier NAT means a phone on mobile data cannot reach a desktop at home directly, and the desktop
usually cannot accept inbound connections either. Commercial "funnels" solve this with their
servers. `cued-funnel` solves it with yours.

```
phone ──TLS──▶ edge :443 ──one outbound TCP control connection (mux)──▶ cuedd :8771 TLS listener ──▶ cuedd :8772
```

## Setup

1. Anywhere with a public address (a small VPS, a Pi behind a forwarded port):
   ```bash
   cued-funnel keygen                 # prints a 64-hex PSK; keep it
   cued-funnel edge --listen 0.0.0.0:443 --control 0.0.0.0:8771 --psk <PSK>
   ```
   Open 443 (phones) and 8771 (the desktop's control connection) on that box's firewall.
2. At home:
   ```bash
   cuedd --funnel edge.example.net:8771 --funnel-psk <PSK> --funnel-public edge.example.net:443
   ```
   On first start `cuedd` makes a self-signed certificate (`funnel-cert.pem` / `funnel-key.pem` in
   the data directory) and logs its SHA-256 fingerprint. The pairing QR carries it as `c=`.
3. Pair the phone as usual. The phone tries the LAN addresses first, then `https://edge…:443` with
   the certificate pinned to that fingerprint.

## What each party can see

| Party | Sees |
|---|---|
| Edge | encrypted TLS bytes and connection metadata (when, how much, from which IP). Never content, never tokens. |
| Desktop | everything, as it should: it is yours. |
| Phone | the desktop, authenticated by the pinned certificate, not by any CA. |

The control connection authenticates the desktop to the edge with `HMAC-SHA256(psk, nonce)`. The
PSK never crosses the wire. Anyone who learns the PSK could attach *their* machine to your edge and
receive your phone's connections, which the phone would then refuse (wrong certificate). Rotate it
with `keygen` if in doubt.

## Framing

`type u8 | stream u32 | len u32 | payload`, types `1 OPEN` (payload: remote address), `2 DATA`,
`3 CLOSE`, `4 PING`, `5 PONG`. Streams are opened by the edge only. One client at a time; a new
client replaces the previous one. See `crates/cued-funnel/README.md`.

## Alternatives that also work

Tailscale, WireGuard or an SSH reverse tunnel in front of `cuedd :8770` do the same job with more
moving parts. The funnel exists so the whole thing stays one binary you can read.

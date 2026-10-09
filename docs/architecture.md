# Architecture

```
                                  ┌────────────────────────── cuedd (Rust, one process) ──────────────────────────┐
  phone (LAN)  ──http/ws──▶ :8770 │ axum router                                                                    │
  browser      ──http/ws──▶       │  /            web UI (embedded ui/)                                            │
                                  │  /api/*       pairing · sync · backup · library · player · stations · settings │
                                  │  /relay       websocket → cued-relay (NIP-01, SQLite)                          │
                                  │  /health /jobs /files   → Python companion worker (spotdl)                     │
  funnel edge ◀──outbound TCP──   │ cued-funnel client ──▶ TLS forwarder :8771 ──▶ :8772 (same router, Origin::External)
                                  │ stations: upstream relay client (mirror in/out), station host                  │
                                  │ player: queue → cued-audio (C++ two decks, crossfade, FFT, BPM)                │
                                  │ SQLite: cued.db (devices, tracks, device_tracks, blobs, follows), relay.db     │
                                  │ files: library/<artist>/<album>/…, backup/<device>/…, downloads/, tmp/*.part   │
                                  └───────────────────────────────────────────────────────────────────────────────┘
```

## Trust boundaries

Two listeners share one router. Requests on the LAN port from a loopback address are **admin** (the
UI). Requests from the funnel arrive on a loopback port too, but through a second listener tagged
`Origin::External`, so they can never be admin. Everything a phone does needs its bearer token;
the token is stored hashed. `/share/<sha>/<cap>` is the one unauthenticated file route and it needs
a 30-minute capability minted by the admin.

## Content addressing

A track is its SHA-256. The phone's manifest lists hashes; the desktop answers with the ones it
lacks; uploads are written to `tmp/<sha>.part`, resumed by `Content-Range`, verified, then moved
into `library/`. Two phones with the same file store it once; `device_tracks` remembers who has
what, and a hash that disappears from a manifest is marked not-present rather than deleted.

## Stations

`cued-relay` is a real relay: any Nostr client can use it. The daemon watches everything it accepts
(`subscribe_all`) to keep the live feed, re-publishes events from paired phones and the desktop to
the upstream relays (`mirror_out`), and subscribes upstream for everyone followed, feeding what
arrives back into the embedded relay (`mirror_in`). A phone that only talks to the desktop still
reaches followers on public relays, and still sees stations that never heard of the desktop.

Hosting from the desktop signs the same kind-30777 `StationState` the phone signs, from the
player's queue, every change and every 30 s.

## Audio

`native/cued-audio` is the phone's `core/dsp` and `core/mix` in C++ with a mixer around it: two
decks pull decoded PCM (miniaudio for wav/flac/mp3, an `ffmpeg` pipe for AAC/m4a), the mixer
applies the chosen crossfade curve, swaps decks at the end of the fade, and feeds a 64-band
streaming spectrogram. The Rust crate compiles it with `cc`, so `cargo build` is the whole build.
`CUED_AUDIO_NULL=1` uses a null device whose clock the daemon ticks: headless boxes and tests.

## Companion

The daemon spawns `python3 -m cued_companion.server` on a loopback port and proxies the phone's
companion API to it, so the phone's existing backend works unchanged. Finished jobs are hashed and
moved into the library like any other file.

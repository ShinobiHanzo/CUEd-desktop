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
`CUED_AUDIO_NULL=1` forces the null device; the daemon also falls back to it when no sound device opens, and ticks its clock itself either way (headless boxes, tests).

## Companion

The daemon spawns `python3 -m cued_companion.server` on a loopback port and proxies the phone's
companion API to it, so the phone's existing backend works unchanged apart from sending its pairing
token (unpaired LAN devices are refused unless `companion_open_lan` is on). Finished jobs are hashed
and moved into the library like any other file.

## Party and ledger

`party.rs` answers signed envelopes from friends on the LAN (search, queue, transport controls) and runs
the UDP beacon on 8772 while a party is on; the handler is transport-agnostic so a phone host speaks the
same protocol over Bluetooth LE. `ledger.rs` summarises the `plays` table; the player writes finished plays
to an outbox that the 250 ms ticker drains into SQLite, so the audio path never touches the database.
Karaoke is a `VocalCut` stage on the mixed output in the C++ engine (`karaoke.cpp`), toggled from the
mini player and persisted in settings.

## Self-update

`update.rs`: a background task asks GitHub Releases for the archive matching the compile-time target
(`CUED_TARGET` from `build.rs`), downloads it with the SHA-256 check and writes `updates/pending/meta.json`.
At startup `at_startup` extracts a staged archive with the system `tar` (bsdtar on Windows opens the zip),
copies `cuedd` (and `cued-funnel` when installed) next to the running binary, renames the old ones to
`.old`, writes `updates/applied.json` and re-executes itself with the same arguments (`exec` on Unix,
spawn-and-exit on Windows). The new version serves `applied.json` through `/api/update` until the notes
are marked seen. Failures are written to `updates/apply-error.txt` and shown in the Updates card; the
old binary keeps running.

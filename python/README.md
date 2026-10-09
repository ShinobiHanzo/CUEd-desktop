# cued_companion — the Python side of the CUEd desktop

Standard-library Python (3.9+), friendly to a Raspberry Pi or an old laptop.
It plays three roles; `docs/protocol.md` at the repo root is the contract for
all of them.

| Role | Module | Entry point |
|---|---|---|
| Download worker `cuedd` spawns | `cued_companion/server.py` | `cued-companion` / `python -m cued_companion.server` |
| Link and Nostr codecs | `cued_companion/links.py`, `cued_companion/nostr.py` | import |
| Phone simulator for testing the desktop | `cued_companion/phone_sim.py` | `cued-phone-sim` / `python -m cued_companion.phone_sim` |

```bash
pip install -e .            # the package and both commands
pip install -e '.[relay]'   # + websockets, only needed by `cued-phone-sim station`
pip install spotdl          # only the download worker needs it (plus ffmpeg, auto-fetched if absent)
```

## 1. Download worker

The companion the phone already knows (`tools/spotdl-server` in the CUEd
repo), made importable and given two extra routes for the Rust daemon.

```bash
python -m cued_companion.server --port 8766 --dir ./downloads --bind 127.0.0.1
```

`cuedd` starts it exactly like that, bound to loopback, and proxies the phone's
requests so the phone's **Downloads → Backend → Companion (LAN)** setting takes
`http://<desktop>:8770` unchanged. Run it with `--bind 0.0.0.0` and it is the
standalone companion again, printing the LAN addresses to type into the app.

| Route | Answer |
|---|---|
| `GET /health` | `"ok"` |
| `GET /` | status page with the LAN addresses |
| `POST /jobs` `{url, format, lrc}` | `{"id"}`; a `url` starting with `-` is refused (400), so no spotdl flag can be smuggled in |
| `GET /jobs/<id>` | `{status, progress, message, files, …}`, status one of `queued running done failed` |
| `GET /jobs` | `{"jobs":[…]}` every job, newest last (new) |
| `DELETE /jobs/<id>` | removes a `done`/`failed` job; `409` while it is queued or running (new) |
| `GET /files/<name>` | the audio or `.lrc` file; bare names only, never outside the download folder |

From Python:

```python
from cued_companion.server import create_server
srv = create_server("127.0.0.1", 0, "downloads", runner=my_runner)   # runner(job, download_dir, emit) -> exit code
threading.Thread(target=srv.serve_forever, daemon=True).start()
```

`runner` defaults to the real spotdl subprocess; tests pass a fake that writes
a file and prints a percentage. Progress is parsed from spotdl's simple TUI
output (`0.05 … 0.90`, then `1.0` when done), as before.

## 2. Codecs

`links.py` encodes and decodes `cued://share`, `cued://station` and
`cued://pair` byte-for-byte like the phone (Java `URLEncoder` with `+` → `%20`),
and builds/verifies the pairing proof `HMAC-SHA256(secret, device|name|nonce)`.

```python
from cued_companion.links import SharePayload, StationLink, PairLink, decode_link, build_pair_request
SharePayload(title="Afterlife (Live) & more", artist="Evanescence", bpm=128.04).encode()
# 'cued://share?v=1&t=Afterlife%20%28Live%29%20%26%20more&a=Evanescence&b=128.0'
```

`nostr.py` is secp256k1 + BIP-340 Schnorr on plain integers (ported from the
phone's `Secp256k1.kt`, checked against the BIP-340 vectors), NIP-01 event ids,
signing and verification, and NIP-19 `npub`/`nsec` bech32.

```python
from cued_companion import nostr
ev = nostr.sign_event(secret_hex, nostr.STATION_KIND, nostr.STATION_TAGS, content)
nostr.verify_event(ev)       # True
nostr.npub(ev["pubkey"])     # 'npub1…'
```

## 3. Phone simulator

Everything the phone does to the desktop, from a shell, so `cuedd` can be
tested end to end without Android. State (bearer token, device id, desktop
URL, station identity) lives in `~/.cued-phone-sim.json` (`--state` to move it).

```bash
cued-phone-sim pair "cued://pair?v=1&n=…&a=http://192.168.1.10:8770&s=…"  [--name "Sim Phone"]
cued-phone-sim sync ~/Music                 # manifest, then resumable chunked uploads of what is missing
cued-phone-sim receive "cued://share?…"      # pulls `f` into ./received like the Receive screen
cued-phone-sim station --relay ws://192.168.1.10:8770/relay --name "Sim" --title "Afterlife" --artist "Evanescence" [--seconds 30]
cued-phone-sim blob put playlists.json ./playlists.json
cued-phone-sim blob get playlists.json ./back.json
cued-phone-sim snapshot                      # POST /api/backup/snapshot
cued-phone-sim status                        # GET  /api/sync/status
```

`pair` tries every LAN address in the link, then the funnel with the TLS cert
pinned to the QR's SHA-256 fingerprint (no CA involved). `sync` hashes each
`.mp3 .m4a .flac .ogg .opus .wav`, reads tags with `ffprobe` when it is on
PATH (otherwise `Artist - Title.ext` and the WAV header), POSTs the manifest,
then for each missing hash does `HEAD` → `X-Cued-Have` and `PUT`s the rest in
1 MiB `Content-Range` chunks with `X-Cued-Meta` on the first. `station` signs a
kind-30777 `StationState` every 10 s and reads the relay's `OK`s. Exit codes:
`0` ok, `1` protocol/usage, `2` network, `3` missing dependency; every failure
is one `error: …` line on stderr. Timeouts everywhere (3 s connect while
pairing, 30 s requests, 120 s uploads).

## Tests

```bash
pip install -r requirements-dev.txt
python -m pytest -q
```

Nothing touches the network or runs spotdl: the server tests use a fake
runner, `tests/fake_desktop.py` is an in-process `http.server` implementation
of the desktop's `/api/pair`, `/api/sync/*`, `/api/backup/*`, `/api/library`
and `/share/*` routes as `protocol.md` describes them (it doubles as
executable documentation of the server side), and the station test runs a
tiny NIP-01 relay in-process when `websockets` is installed. `tests/test_e2e.py`
runs only against a live daemon:

```bash
CUEDD_URL=http://127.0.0.1:8770 CUEDD_PAIR_LINK="cued://pair?…" python -m pytest tests/test_e2e.py
```

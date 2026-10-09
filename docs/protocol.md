# Phone ↔ desktop protocol

Everything the phone and the desktop say to each other, in one place. The
desktop is `cuedd`, one process on one port (default **8770**). Nothing here
needs an account or a third party: pairing is a QR code, transport is HTTP on
the LAN or the user's own funnel from outside, identity is the same
secp256k1 key the phone already uses for Stations.

Field names are short on purpose: the pair link has to fit in a QR code the
phone's camera reads at arm's length, like `cued://share`.

## 1. Links the phone understands

| Link | Who shows it | What it does |
|---|---|---|
| `cued://share?v=1&t=&a=&l=&f=&k=&g=&b=` | either side | one track (today's protocol, unchanged; see CUEd `docs/sharing.md`) |
| `cued://station?p=&n=&r=` | either side | follow a station (unchanged) |
| `cued://pair?v=1&n=&a=&f=&c=&s=&k=&r=` | desktop | link this phone with this desktop (new, below) |

The desktop decodes and produces all three. `cued://share` with an `f` pointing
at the desktop works with the phone app **as shipped today**: that is how the
desktop pushes a track to a phone before the phone learns `cued://pair`.

## 2. Pairing (`cued://pair`)

The desktop shows a QR code. The link inside it:

```
cued://pair?v=1
  &n=<desktop name>                       e.g. "Idris's laptop"
  &a=<http://ip:port,http://ip:port>      every LAN address cuedd listens on, comma separated
  &f=<https://host:port>                  funnel address reachable from the internet (optional)
  &c=<sha256 hex of the funnel TLS cert>  pin for the self-signed cert behind `f` (present iff `f` is)
  &s=<32 hex = 16 random bytes>           one-time pairing secret, valid 10 minutes
  &k=<64 hex>                             desktop's station pubkey (its Nostr identity)
  &r=<ws://ip:port/relay>                 the desktop's embedded relay, so the phone can add it
```

Phone side, after scanning:

1. Try each `a` in order (connect timeout 3 s), then `f` with the cert pinned to `c`.
2. `POST {base}/api/pair` with JSON

   ```json
   {"v":1,"name":"Pixel 8","device":"<16 hex, random, stable per phone>",
    "pubkey":"<phone station pubkey hex or empty>",
    "nonce":"<16 hex random>",
    "proof":"<HMAC-SHA256(key = bytes(s), msg = device + '|' + name + '|' + nonce) hex>"}
   ```

   The proof shows the phone saw the QR; the secret itself never crosses the
   wire. One secret pairs one phone; the desktop then forgets it.

3. `200` →

   ```json
   {"token":"<bearer, 32 bytes base64url>","desktop":{"name":"…","pubkey":"…",
    "relay":"ws://…/relay","funnel":"wss://…/relay or null","cert":"<sha256 hex or null>"}}
   ```

   `403` means wrong or expired secret; `409` means that device id is already
   paired (unlink it on the desktop first).

Every later request carries `Authorization: Bearer <token>`. Tokens live until
the device is unlinked in the desktop UI.

Links can also be pasted: the phone's Receive screen paste box, and the
desktop UI's "Link a phone" page, both accept the raw `cued://` text.

Desktop side (admin, loopback only): `POST /api/pair/new` mints a secret and
returns `{"link","svg","addresses","funnel","expiresInSec"}`; `GET /api/devices`
lists paired phones with their backup status; `DELETE /api/devices/{id}`
unlinks one. The addresses in `a` are the machine's private IPv4 addresses,
Wi-Fi first; a box without one (a VPS, a container) advertises its other
addresses, and `CUED_ADVERTISE=host1,host2` overrides the list.

## 3. Sync, backup, stream (bearer)

All JSON unless noted. Tracks are addressed by the SHA-256 of the file bytes:
content-addressed, so the same file from two phones is stored once.

### Manifest

`POST /api/sync/manifest`

```json
{"tracks":[{"key":"<phone-side id>","title":"","artist":"","album":"",
            "durationMs":0,"size":0,"sha256":"<64 hex>","mime":"audio/mp4",
            "genres":["…"],"bpm":128.0,"link":"https://open.spotify.com/…",
            "modified":1700000000000}]}
```

→ `{"missing":["<sha256>",…],"known":123}`. The desktop keeps the newest
manifest per device: it is the backup index ("what was on the phone") and
tells the phone which files to upload. A track that disappears from the
manifest is **not** deleted on the desktop; it is marked as no longer on that
phone.

### Upload (phone → desktop), resumable

- `HEAD /api/sync/track/{sha256}` → `200` with header `X-Cued-Have: <bytes stored so far>`;
  `204` when the whole file is already there.
- `PUT /api/sync/track/{sha256}` body = bytes. Optional `Content-Range: bytes <start>-<end>/<total>`
  to resume from `start`; without it the upload starts over. Header
  `X-Cued-Meta: <the manifest entry as JSON>` on the first chunk.
  → `201` when the file is complete and the hash matched, `202` when more
  bytes are expected, `422` when the hash does not match (the partial file is
  dropped).

Files land under `<data>/library/<artist>/<album>/<artist> - <title>.<ext>` and
are indexed in SQLite with the device(s) they came from.

### Download / stream (desktop → phone or browser)

- `GET /api/track/{sha256}` with HTTP `Range` support, `Accept-Ranges: bytes`,
  the right `Content-Type`. This is what the phone plays when it streams the
  desktop library instead of copying it.
- `GET /api/track/{sha256}/meta` → the manifest-shaped entry plus `devices`.
- `GET /api/library?q=&device=&offset=&limit=` → `{"tracks":[…],"total":n}`.

### Push to a phone that has not paired (works today)

`POST /api/share/{sha256}` → `{"link":"cued://share?…&f=http://…/share/<sha256>/<cap>","web":"https://shinobihanzo.github.io/CUEd/#…","svg":"<qr svg>"}`.
`cap` is a random capability token valid 30 minutes; the file route under
`/share/` needs no bearer, so the phone's existing Receive screen pulls it.

### Pull from a phone's Share screen (works today)

`POST /api/receive {"text":"cued://share?…"}` decodes the payload, fetches `f`
(and `/meta/<id>` next to it when the sender is a CUEd share server), stores
the file and answers with the library entry.

### Backup snapshots and blobs

- `POST /api/backup/snapshot` → freezes the device's current manifest as
  `<data>/backup/<device>/<ISO time>.json`; `GET /api/backup/snapshots` lists them.
- `PUT /api/backup/blob/{name}` / `GET …` / `GET /api/backup/blobs`: opaque
  files the phone wants kept (playlists export, settings, an **encrypted**
  station key). The desktop never sees inside; names are `[A-Za-z0-9._-]{1,80}`.

### Status

`GET /api/sync/status` → per device: tracks on phone, tracks backed up, bytes,
last manifest time, last upload time.

## 4. Stations relayed through the desktop

`cuedd` embeds a **Nostr relay** at `GET /relay` (WebSocket; `ws://` on the LAN,
`wss://` through the funnel). It speaks NIP-01: `EVENT`, `REQ`, `CLOSE` in;
`EVENT`, `EOSE`, `OK`, `CLOSED`, `NOTICE` out. Filters: `ids`, `authors`,
`kinds`, `#<single-letter tag>`, `since`, `until`, `limit`. Replaceable kinds
(0, 3, 10000–19999) and parameterised-replaceable kinds (30000–39999, keyed by
the `d` tag) keep only the newest per author. Every event's id and BIP-340
signature are checked before it is stored. CUEd station states are kind
`30777`, `d` = `cued`, exactly as the phone publishes them.

Put `ws://<desktop>:8770/relay` (or the funnel `wss://…/relay`) in the phone's
Stations → relays list and the phone's social traffic goes through the
desktop. Optionally the desktop **mirrors**: events it accepts from a paired
device are re-published to the upstream relays in its settings, and it
subscribes upstream for every followed pubkey so a listener on the LAN sees
the world through one socket. The desktop is a relay, not an authority: it
cannot forge anything, and any other relay can replace it.

The desktop also has its own identity (`<data>/identity.json`, mode 0600,
`nsec` exportable) and can host a station from its own player or follow
stations itself (`/api/stations/*`).

## 5. Self-hosted funnel

`cued-funnel` is one small binary you run on any box with a public address
(a $3 VPS, a Pi behind a forwarded port). It never sees your traffic in the
clear:

```
phone ──TLS──▶ edge :443 ──mux over one TCP control connection──▶ cuedd :8771 (TLS listener)
```

- `cuedd` connects **outbound** to the edge's control port and authenticates
  with a pre-shared key (HMAC challenge). No inbound port is opened at home.
- Every public connection the edge accepts becomes a stream on that
  connection and is forwarded byte for byte to `cuedd`'s TLS listener. TLS
  terminates on the desktop with its own self-signed certificate; the QR
  carries its fingerprint (`c`), the phone pins it. The edge only ever sees
  ciphertext.
- Mux framing: `type u8 | stream u32 | len u32 | payload`. Types: `1 OPEN`,
  `2 DATA`, `3 CLOSE`, `4 PING`, `5 PONG`. Streams are opened by the edge
  only. Handshake: edge sends 32 random bytes; client answers
  `HMAC-SHA256(psk, nonce)`; edge replies `OK` or closes.

See `docs/funnel.md` for setup.

## 6. Companion downloads (unchanged)

`cuedd` serves the companion API from `tools/spotdl-server` on its own port, so
the phone's Downloads → Backend → **Companion (LAN)** setting takes
`http://<desktop>:8770` as is: `GET /health`, `POST /jobs`, `GET /jobs/{id}`,
`GET /files/{name}`. Jobs run in the Python worker (`python/cued_companion`),
which `cuedd` starts and watches; the finished files are also added to the
desktop library.

## 7. Trust model, said plainly

- LAN requests are plain HTTP with a bearer token, the same model as the
  phone's own share server and the existing companion. Anyone on your Wi-Fi
  who can read packets could read a token; pair at home, not at the airport.
- From outside, everything goes through TLS to a cert you pinned.
- The desktop holds files you chose to back up and a Nostr key pair. Both are
  plain files under the data directory, yours to move or delete.
- Nothing calls home. Upstream relays are the ones you listed.

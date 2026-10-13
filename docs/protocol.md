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

## 6. Companion downloads

`cuedd` serves the companion API from `tools/spotdl-server` on its own port, so
the phone's Downloads → Backend → **Companion (LAN)** setting takes
`http://<desktop>:8770` as is: `GET /health`, `POST /jobs`, `GET /jobs/{id}`,
`GET /files/{name}`. Jobs run in the Python worker (`python/cued_companion`),
which `cuedd` starts and watches; the finished files are also added to the
desktop library.

Since 0.2.4 everything but `/health` needs a caller the daemon knows: the
desktop UI, or a paired phone's `Authorization: Bearer <token>` (the phone
sends the token of the desktop whose LAN address matches the companion URL).
Anything else on the LAN gets `401` unless the setting `companion_open_lan`
(*Let unpaired devices on this Wi-Fi use the downloader*, off) is on.

## 7. Who may do what: the three tiers

Code injection through the API is ruled out by construction: nothing that
changes state is reachable from outside the LAN, and reads from outside need
a biometric unlock on the phone every few minutes.

The **admin** tier (the desktop UI) is a loopback peer that also addressed the
daemon by a loopback name (`Host: localhost` / `127.0.0.1` / `[::1]`), so a
DNS name an attacker points at `127.0.0.1` does not count. A browser write
(`POST`/`PUT`/`DELETE`) is refused when `Sec-Fetch-Site` is not
`same-origin`/`none` or `Origin` differs from `Host`; non-browser clients send
neither header and are unaffected. The daemon sends no CORS headers: the UI is
same-origin and no other page may read its answers. Request bodies are capped
at axum's 2 MB except the two uploads (`PUT /api/sync/track/{sha}`,
`PUT /api/backup/blob/{name}`, 4 GiB). A biometric challenge lives 120 s and
is consumed by the first valid assertion; the assertion itself is good for
10 min.

| Tier | Who | Reaches the desktop how | Allowed |
|---|---|---|---|
| **Admin** | the person at the desktop | the LAN listener from a loopback address (the web UI) | everything |
| **Phone on the LAN** | a paired phone | the LAN listener, from a private IPv4 (10/8, 172.16/12, 192.168/16) or loopback, `Authorization: Bearer` | everything a phone may do: sync, upload, settings, friends, listening |
| **Phone away from home** | a paired phone | the funnel (`Origin::External`), `Authorization: Bearer` **and** `X-Cued-Assertion` | `GET`/`HEAD` on the read-only list below, and the relay websocket when the relay is on |

Anything else is refused with `403 {"error":"writes only on the LAN"}` before
a handler runs. A phone on a public IP that somehow reaches the LAN port is
treated as remote too. There is no third path: no remote admin, no API key.

**Read-only list** (remote): `/api/me`, `/api/library`, `/api/track/{sha}`
(+ `/meta`, `/cover`, `/lyrics`), `/api/sync/status`, `/api/backup/snapshots`,
`/api/backup/blobs`, `/api/backup/blob/{name}`, `/api/stations/feed`,
`/api/stations/follows`, `/api/friends`, `/api/friends/requests`,
`/api/account`, `/api/account/chain`, `/api/settings/shared`, `/api/auth/challenge`.

### Biometric assertion

At pairing the phone registers `biokey`: an ECDSA P-256 public key made in
the Android Keystore with `setUserAuthenticationRequired(true)` (biometric or
device credential), 65-byte uncompressed SEC1, hex. The private half can
only sign after the person unlocks.

1. `GET /api/auth/challenge` (bearer, any tier) →
   `{"challenge":"<32 hex>","expiresInSec":120}`.
2. The phone signs `cued-bio|<device>|<challenge>` (ASCII, SHA-256, ECDSA
   P-256) after a biometric prompt.
3. Every remote request carries `X-Cued-Assertion: <challenge>.<sig hex>`.
   `sig` is 64 bytes `r||s`, or the Keystore's DER form; both are accepted.
   An assertion is good for 10 minutes, then the phone unlocks again.

A device that registered no `biokey` cannot read remotely at all. The desktop
never sees a fingerprint or a face; it sees a signature from a key the phone
refuses to use without one.

## 8. Accounts: one key, every device

An account **is** a secp256k1 key pair, the same one Stations already use.
Its id is `sha256(pubkey)` hex, and what other people see is only the
public key (`npub…`) and a name. The private key lives on each device the
account is bound to and nowhere else.

### The account chain

Every account keeps a small signed, hash-linked chain of blocks, held in
full on each of its devices and shipped between them on the LAN or through
the relay. Only the holder of the private key can extend it, so no server,
no consensus and no coin are involved: it is self-custody with a tamper-
evident history, and it is what lets you walk up to a new computer and be
the same person.

```json
{"v":1,"height":3,"prev":"<hash of block 2>","ts":1700000000,
 "type":"bind_device","data":{"devhash":"<64 hex>","name":"Pixel 8","kind":"phone"},
 "pubkey":"<64 hex>","hash":"<sha256 of canonical [v,height,prev,ts,type,data,pubkey]>",
 "sig":"<BIP-340 over hash>"}
```

Block types: `create {name}` (height 0, `prev` empty), `profile {name}`,
`bind_device {devhash,name,kind}`, `unbind_device {devhash}`,
`settings {…}` (the shared settings, §11). `devhash` is
`sha256("cued-device|" + device id + "|" + per-install random salt)`: a
unique hash of the device that reveals nothing about it.

Validity: heights are consecutive, `prev` matches, every block is signed by
the key of block 0. `data` is hashed exactly as its author wrote it, so every
implementation keeps object keys in their original order (serde_json with
`preserve_order`, Python dicts, Kotlin `JsonObject`); none may sort them. Merging two copies: the longer valid chain wins; equal
height, the lower tip hash wins; the loser's extra blocks are re-appended
on top when they still apply. `GET /api/account/chain` is read-only (remote
allowed); `POST /api/account/chain {"blocks":[…]}` (LAN) merges.

### Pairing and accounts

`PairRequest` gains three fields:

```json
{"…":"…","account":"join|keep|separate","biokey":"<130 hex>","devhash":"<64 hex>"}
```

| `account` | Meaning | What crosses the wire |
|---|---|---|
| `join` | the phone takes the desktop's account | the pair response carries `"account":{"sealed":"…"}`: the desktop's secret key and chain, sealed with the pairing secret |
| `keep` | the desktop takes the phone's account | the phone then `POST /api/account/import {"sealed":"…"}` on the LAN; the desktop replaces its key and chain, binds itself with a `bind_device` block |
| `separate` | two accounts, paired devices | nothing |

A desktop that has never had an account answers `join` with a fresh one it
makes on the spot. "Scan the same connect-your-phone code on another
computer and choose new or keep" is exactly `join` versus `keep`.

### Seal

The pairing secret never crosses the wire, so a key derived from it is a
private channel. `seal(secret, label, plaintext)`:

```
k   = HMAC-SHA256(key = "cued-seal-v1", msg = secret bytes || label)
ke  = HMAC-SHA256(k, "enc");  km = HMAC-SHA256(k, "mac")
nonce = 16 random bytes
keystream block i (0-based) = HMAC-SHA256(ke, nonce || i as u32 big-endian)
ct  = plaintext XOR keystream
tag = HMAC-SHA256(km, nonce || ct)
sealed = base64url(nonce || ct || tag)
```

Plain HMAC in counter mode with an encrypt-then-MAC tag: every platform here
has HMAC-SHA256 in its standard library, and all of it fits on one screen.
Label is `account` for the account bundle.

## 9. Friends

A friend is a public key you chose to trust with your station and your
presence. Friend lists live on your devices; what leaves is only your public
key, your name, and a signed "request/accept/remove".

- Link: `cued://friend?p=<pubkey>&n=<name>&r=<relays>` shown as a QR code
  and offered over NFC exactly like a share. Scanning someone's link sends
  them a request; two people scanning each other are friends at once.
- A share can carry a friend request: `cued://share?…&fr=<pubkey>&fn=<name>`
  (the "send a friend request" toggle on the sharing page). Receivers on
  older builds ignore the fields.
- Over relays, the request is a Nostr event, kind `30778`, `d` =
  `friend:<target pubkey>`, tags `["p", target]`, `["t","cued-friend"]`,
  content `{"v":1,"name":"…","status":"request|accept|remove","ts":…}`,
  signed by the sender. Each side subscribes to `#p` = its own key.
- States on each side: `pending_out`, `pending_in`, `friends`. Friends are
  followed as stations automatically.
- API: `GET /api/friends`, `GET /api/friends/requests`, `GET /api/friends/link`,
  `POST /api/friends {"text":"<link or npub>"}`, `POST /api/friends/{pubkey}/accept`,
  `DELETE /api/friends/{pubkey}`.

## 10. Listening in

A station is metadata only: the current track and the next three, nothing
more, ever (`StationTrack` in §4). To let friends *hear* it, the host's
desktop adds, when its *friend streaming* setting is on, three capability
URLs to each `StationTrack`: `stream`, `cover`, `lyrics`, each
`https://<host>/share/<sha>/<cap>[/cover|/lyrics]`, valid for two hours and
minted only while the track is in the current-or-next-three window.

A listener (phone or desktop) resolves every entry in this order:

1. a track already in its own library (title and artist match),
2. the host's `stream` URL, saved as a temporary file,
3. the `link` through its downloader, as a temporary file.

Then it plays at `now − startedAt`, nudging only when more than three
seconds off: a few seconds between friends is fine. Temporary audio,
thumbnails, lyrics and metadata are kept for **48 hours** then evicted, so a
listener can look back at what was on. On the desktop: `POST /api/stations/listen
{"pubkey":"…"}`, `DELETE /api/stations/listen`, `GET /api/stations/listen`,
cache under `<data>/cache/stations/`.

Cover art and lyrics come from `GET /api/track/{sha}/cover` (the embedded
picture) and `GET /api/track/{sha}/lyrics` (`.lrc` sidecar, else the embedded
lyrics tag), both read-only.

## 11. Shared settings and the relay switch

Settings that should be the same on every device of an account live in the
latest `settings` block of the account chain:

```json
{"relayEnabled":false,"friendStreaming":true,"theme":{"bg":"#0e0f13","panel":"#161820","text":"#ececf1","accent":"#5ef2c6","accent2":"#7aa2ff","danger":"#ff6b7a"},"stationName":""}
```

`GET /api/settings/shared` (read-only) and `PUT /api/settings/shared` (LAN or
admin, partial object) append a block and notify every open UI. **The
desktop relay is off by default.** While off, `/relay` answers `403` and
nothing is mirrored; turning it on from any device turns it on everywhere.

## 12. Trust model, said plainly

- Writes happen only on your own Wi-Fi, from a device you paired, with its
  token. From outside, the desktop is read-only, and only after the phone's
  biometric unlock.
- The account's private key is on your devices and in sealed transfers
  between them; the seal key comes from a QR code only the two devices saw.
- Friends and stations see public keys, names and track metadata. Audio
  moves only through capability URLs you turned on, to friends, for the
  tracks you are playing now and next.
- Nothing calls home. Upstream relays are the ones you listed, and the
  desktop's own relay stays off until you switch it on.

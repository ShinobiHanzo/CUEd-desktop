# Changelog

## 0.3.0

Three things Spotify does not do, all local:

- **Listening ledger** (new *Ledger* tab): plays, listening time, discoveries, streaks, hour-of-day and
  weekday shapes, top artists / tracks / albums / genres, for this month, last month, 7 or 30 days, the
  year or all time. Computed on the box from a `plays` table the player fills; export the raw log as a
  `cued-ledger/1` JSON file you own, or forget it with one click. The `ledger` setting stops recording.
- **Party: a shared queue for friends.** Friends on the Wi-Fi find this desktop through a UDP beacon
  (8772), then search the library and queue songs, each request signed with their station key; only
  people in the friends list get through (protocol §13). *Guests may skip, pause and resume* is a second
  switch. The Friends tab shows who queued what, live. `python -m cued_companion.party` is a
  command-line guest.
- **Karaoke** (K on the mini player, strength in Settings): cancels the centre of the mix, the lead
  vocal, while keeping the bass; two-stage filter so a 1 kHz voice leaks at -32 dB, not -16. Works on
  stereo mixes with centred vocals; wide-panned backing vocals survive, mono files go quiet.

## 0.2.5

- Linux builds: the 0.2.4 archives for Linux x86_64 and arm64 never appeared because GCC 11 on
  Ubuntu 22.04 (the new, lower glibc build base) wants `<cstddef>` included explicitly wherever
  `size_t` is used; it is now. No other change: everything listed under 0.2.4 applies.

## 0.2.4

Security on the LAN (all three were reachable by any device on the Wi-Fi, or by any web page
open in the desktop's browser):

- **No more wildcard CORS.** The UI is same-origin; the permissive CORS layer that let any web
  page read answers from `127.0.0.1:8770` (and so mint itself a pairing token) is gone. The admin
  tier now also requires the `Host` header to be loopback (DNS rebinding) and refuses cross-site
  `POST`/`PUT`/`DELETE` by `Origin` / `Sec-Fetch-Site`.
- **The download worker needs a paired phone's token** (or the desktop UI). CUEd 0.2.6 on the
  phone sends it when the companion address matches a paired desktop. A new setting, *Let unpaired
  devices on this Wi-Fi use the downloader* (off), restores the old open behaviour for older phones
  and other clients.
- **Body limits.** The 4 GiB request limit applies to the two upload routes only; every JSON route
  is back to axum's 2 MB, so one unauthenticated request can no longer exhaust memory.
- `/api/status` and `/api/funnel` need the UI or a token; a biometric challenge lives 120 s and is
  consumed by its first valid assertion; the private key file is created `0600` from the first
  byte.

Fixes and platforms:

- **ffmpeg fallback decoder:** a 30-second ring buffer instead of the whole decoded track in
  memory (a two-hour mix used to cost about 1.4 GB and stall the audio thread); seeks outside the
  ring restart ffmpeg with `-ss`.
- **Headless boxes:** when no sound device opens, the player now knows it (position advances, the
  station host's clock moves) instead of needing `CUED_AUDIO_NULL`.
- **Hostname on macOS** (and anywhere without `/etc/hostname`) via the `hostname` command, so a
  Mac no longer pairs as "CUEd desktop".
- **Release archives for six targets:** Linux x86_64 and arm64 (built on Ubuntu 22.04 for a low
  glibc floor; Raspberry Pi OS bookworm included), macOS arm64 and x86_64, Windows x86_64 and
  arm64. Only Linux x86_64 has been exercised end to end.

## 0.2.3

- Windows: the ffmpeg fallback decoder spawned its child with fork/exec and `sys/wait.h`, so the
  engine did not compile with MSVC; it now uses `CreateProcess` with an inheritable pipe there
  (same code path otherwise). MSVC builds with `/EHsc`. 0.2.2 fixed the build script's path
  problem but still shipped without a Windows archive.
- CI: `workflow_dispatch` gained a *dry run* switch that builds every platform's archive without
  tagging or publishing.

## 0.2.2

- Windows release build: the audio engine's build script fed `cl.exe` a verbatim `\\?\` path
  from `canonicalize()`, which it cannot open; the prefix is stripped now. 0.2.1 shipped
  Linux and macOS archives only.

## 0.2.1

- **Three tiers.** Nothing that changes state is reachable from outside the LAN: admin is the
  UI on localhost, phones write only from a private address with their token, and from the
  funnel the desktop is read-only and every read needs a fresh biometric assertion (ECDSA
  P-256 key the phone keeps behind `BiometricPrompt`). Capability share links stay readable.
- **Accounts on a chain.** An account is the secp256k1 key; its signed, hash-linked chain
  (`create`, `bind_device`, `unbind_device`, `profile`, `settings`) lives on every device and
  merges "longer valid chain wins". Pairing offers *join* (phone takes the desktop's account,
  sealed with the pairing secret), *keep* (desktop takes the phone's) or *separate*.
  Only public keys are visible to anyone else; the private key is exported only by the admin.
- **Friends.** `cued://friend` QR/NFC links, kind-30778 request/accept/remove events, a
  "send a friend request with it" toggle on share links (`fr`/`fn`), friends list and
  requests on the desktop (API for the phone).
- **Listening in.** Stations stay metadata only (current + next three). With *friend
  streaming* on, the host adds capability URLs for audio, cover and lyrics; a listener
  resolves library → host stream → downloader into temporary files, follows the host within
  three seconds, and keeps temp audio, thumbnails, lyrics and metadata for 48 hours.
- **Shared settings.** Relay switch (off by default), friend streaming, theme colours and
  station name live in the account chain and sync to every device.
- **UI.** Floating 150×150 mini player with the spectrograph at 30 % over the cover; the
  italic *L* expands it to 150×400 with a blurred cover and synced lyrics. Settings got
  sub-pages: General, Theme colours (presets, live preview), Account, Relay & privacy.
  New Friends tab. Cover art and lyrics endpoints.
- Phone repo: `FriendLink`, `Seal`, `AccountChain` codecs in `core`, share friend fields,
  `docs/desktop.md` round-2 list.
- Release archives for Linux, macOS and Windows published from CI on every `v*` tag (or a
  manual run with a tag name), with the changelog section as the notes.
- Phone app brought to parity: pairing with the account choice, biometric reads, backup,
  streaming the desktop library, friends, listening in, shared settings, mini player.

## 0.1.0

First cut: pairing over QR, manifest sync with resumable content-addressed uploads, range
streaming, share-to-phone, companion downloads, embedded Nostr relay, station feed and
hosting, self-hosted funnel with a pinned certificate, C++ two-deck engine, web UI.

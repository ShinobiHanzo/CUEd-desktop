# Changelog

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

## 0.1.0

First cut: pairing over QR, manifest sync with resumable content-addressed uploads, range
streaming, share-to-phone, companion downloads, embedded Nostr relay, station feed and
hosting, self-hosted funnel with a pinned certificate, C++ two-deck engine, web UI.

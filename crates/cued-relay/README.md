# cued-relay

The NIP-01 Nostr relay `cuedd` embeds at `GET /relay` (`docs/protocol.md` §4).
It is a library with no transport of its own: the daemon owns the websocket
and drives one `Connection` per socket.

## Using it

```rust
let relay: Arc<Relay> = Relay::open(Some(&data_dir.join("relay.sqlite")))?; // None = in memory

// per websocket:
let mut conn = relay.connect();
loop {
    tokio::select! {
        Some(Ok(Message::Text(text))) = socket.next() => {
            for reply in conn.handle(&text) { socket.send(Message::Text(reply)).await?; }
        }
        Some(frame) = conn.next_push() => socket.send(Message::Text(frame)).await?,
        else => break,
    }
}
// dropping `conn` closes its subscriptions
```

`tests/websocket.rs` is a complete tungstenite server written this way.

Other entry points the daemon uses:

- `relay.publish(&event) -> Accept` — same pipeline as an inbound `EVENT`
  (`Stored`, `Duplicate(msg)`, `Rejected(msg)`, `Ephemeral`); for mirroring
  events pulled from upstream relays.
- `relay.subscribe_all() -> broadcast::Receiver<Event>` — every accepted event.
- `relay.query(&[filter_json]) -> Vec<Event>`, `relay.latest_by_author(kind, pubkey, d_tag)`.
- `relay.stats() -> RelayStats { events, connections, subscriptions, bytes }`.

## Rules

- Every event's id is recomputed and its BIP-340 signature checked.
  Rejections: `invalid: id mismatch`, `invalid: bad signature`,
  `error: created_at too far in the future` (> 15 min), `blocked: event too large` (> 64 KiB).
- Replaceable kinds (0, 3, 10000–19999) keep the newest per author;
  parameterised-replaceable kinds (30000–39999) the newest per author and `d`
  tag. Older arrivals get `OK true "duplicate: …"` and are not stored.
- Ephemeral kinds (20000–29999) are pushed to live subscriptions, never stored.
- Filters: `ids`, `authors`, `kinds`, `#<tag>`, `since`, `until`, `limit`
  (default 500, cap 1000); several filters in one `REQ` are ORed. At most 20
  subscriptions per connection, subscription ids up to 64 characters.
- Storage: SQLite (`events` + `tags` tables, WAL on disk).

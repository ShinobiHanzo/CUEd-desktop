//! The same BIP-340 vectors the phone's `StationCryptoTest` runs, plus a
//! cross-check that a desktop-signed event is what the phone would produce.

use cued_proto::nostr::{self, parse_client_message, parse_relay_message, ClientMessage, RelayMessage};
use cued_proto::{decode_link, nip19, station, Identity, Link, StationState, StationTrack};

fn vectors() -> Vec<Vec<String>> {
    include_str!("bip340-vectors.csv").lines().skip(1).filter(|l| !l.trim().is_empty()).map(|l| l.split(',').map(str::to_string).collect()).collect()
}

#[test]
fn bip340_sign_vectors() {
    let mut signed = 0;
    for v in vectors() {
        let (idx, sk, pk, aux, msg, sig, ok) = (&v[0], &v[1], &v[2], &v[3], &v[4], &v[5], v[6] == "TRUE");
        if sk.is_empty() {
            continue;
        }
        let id = Identity::from_secret_hex(sk).unwrap();
        assert_eq!(pk.to_lowercase(), id.pubkey_hex(), "pubkey #{idx}");
        if ok {
            let aux: [u8; 32] = hex::decode(aux).unwrap().try_into().unwrap();
            let got = id.sign_raw(&hex::decode(msg).unwrap(), &aux);
            assert_eq!(sig.to_lowercase(), hex::encode(got), "sig #{idx}");
            signed += 1;
        }
    }
    assert!(signed >= 4);
}

#[test]
fn bip340_verify_vectors() {
    let mut checked = 0;
    for v in vectors() {
        let (idx, pk, msg, sig, ok) = (&v[0], &v[2], &v[4], &v[5], v[6] == "TRUE");
        if pk.len() != 64 || sig.len() != 128 {
            continue;
        }
        assert_eq!(ok, nostr::verify_schnorr(msg, pk, sig), "verify #{idx}");
        checked += 1;
    }
    assert!(checked >= 10);
}

#[test]
fn nip19_round_trip() {
    let id = Identity::generate();
    assert!(id.npub().starts_with("npub1"));
    assert_eq!(nip19::parse(&id.npub()), Some(("npub", id.pubkey_hex())));
    assert_eq!(nip19::parse(&id.nsec()), Some(("nsec", id.secret_hex())));
    assert_eq!(nip19::parse(&id.pubkey_hex().to_uppercase()), Some(("hex", id.pubkey_hex())));
    assert_eq!(nip19::parse("npub1notvalid"), None);
    // NIP-19's own example, also in the phone's tests.
    assert_eq!(nip19::npub("3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"), "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6");
    assert_eq!(nip19::parse("npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w7"), None);
    assert_eq!(Identity::from_secret(&id.nsec()).unwrap().pubkey_hex(), id.pubkey_hex());
    assert!(Identity::from_secret(&id.npub()).is_err());
}

#[test]
fn canonical_form_and_known_id() {
    // Computed independently (Python: hashlib.sha256(json.dumps([0,pk,1700000000,30777,[["d","cued"],["t","cued-station"]],"{}"],separators=(",",":")).encode()).hexdigest()).
    let pk = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
    let c = nostr::canonical(pk, 1_700_000_000, station::KIND, &station::tags(), "{}");
    assert_eq!(c, format!(r#"[0,"{pk}",1700000000,30777,[["d","cued"],["t","cued-station"]],"{{}}"]"#));
    assert_eq!(nostr::id(pk, 1_700_000_000, station::KIND, &station::tags(), "{}"), "b9b19a4064a063053eaa2e382fdf36ecb94a4e777dc3df01871177d43905e72a");
    // Escaping of quotes, backslashes and newlines follows NIP-01 / JSON: both sides use their stock JSON encoder.
    let c2 = nostr::canonical(pk, 1, 1, &[], "a \"quoted\" \\ line\nnext é");
    assert_eq!(c2, format!(r#"[0,"{pk}",1,1,[],"a \"quoted\" \\ line\nnext é"]"#));
}

#[test]
fn events_sign_verify_and_serialise() {
    let id = Identity::generate();
    let state = StationState {
        name: "Idris".into(),
        status: station::STATUS_ON_AIR.into(),
        now: Some(StationTrack { title: "Afterlife".into(), artist: "Evanescence".into(), key: "k1".into(), ..Default::default() }),
        started_at: 1_700_000_000_000,
        seq: 3,
        ..Default::default()
    };
    let ev = id.sign(station::KIND, station::tags(), state.encode(), 1_700_000_000);
    assert!(ev.verify());
    let mut bad = ev.clone();
    bad.content.push(' ');
    assert!(!bad.verify());
    assert_eq!(ev.tag("d"), Some("cued"));
    assert!(ev.is_param_replaceable());

    let wire = nostr::relay_event_message("sub", &ev);
    match parse_relay_message(&wire) {
        RelayMessage::Event { sub_id, event } => {
            assert_eq!(sub_id, "sub");
            assert_eq!(event, ev);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(StationState::decode(&ev.content).unwrap(), state);
    assert_eq!(parse_relay_message(r#"["OK","abc",true,""]"#), RelayMessage::Ok { event_id: "abc".into(), accepted: true, message: String::new() });
    assert!(matches!(parse_relay_message("garbage"), RelayMessage::Unknown(_)));

    match parse_client_message(&nostr::event_message(&ev)) {
        ClientMessage::Event(e) => assert_eq!(e, ev),
        other => panic!("{other:?}"),
    }
    match parse_client_message(&nostr::req_message("s1", &[nostr::station_filter(&[id.pubkey_hex()], Some(5))])) {
        ClientMessage::Req { sub_id, filters } => {
            assert_eq!(sub_id, "s1");
            assert_eq!(filters[0]["kinds"][0], 30777);
            assert_eq!(filters[0]["since"], 5);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn link_dispatch() {
    assert!(matches!(decode_link("cued://share?v=1&t=A&a=B"), Ok(Link::Share(_))));
    assert!(matches!(decode_link(&format!("cued://station?p={}", "ab".repeat(32))), Ok(Link::Station(_))));
    assert!(matches!(decode_link(&format!("cued://pair?v=1&n=x&a=http%3A%2F%2F1.2.3.4%3A8770&s={}", "0f".repeat(16))), Ok(Link::Pair(_))));
    assert!(decode_link("https://example.com").is_err());
}

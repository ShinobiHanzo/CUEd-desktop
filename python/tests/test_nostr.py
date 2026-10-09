import csv
import json
import os

import pytest

from cued_companion import nostr

HERE = os.path.dirname(os.path.abspath(__file__))
PUB = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"


def vectors():
    with open(os.path.join(HERE, "bip340-vectors.csv"), newline="") as f:
        return list(csv.DictReader(f))


VECTORS = vectors()


def test_vector_file_present():
    assert len(VECTORS) == 19


@pytest.mark.parametrize("row", VECTORS, ids=[r["index"] for r in VECTORS])
def test_bip340_vector(row):
    pub = bytes.fromhex(row["public key"])
    msg = bytes.fromhex(row["message"])
    sig = bytes.fromhex(row["signature"])
    expected = row["verification result"] == "TRUE"
    assert nostr.verify(msg, pub, sig) is expected, row["comment"]
    if row["secret key"]:
        sk = bytes.fromhex(row["secret key"])
        assert nostr.public_key(sk) == pub
        assert nostr.sign(msg, sk, bytes.fromhex(row["aux_rand"])) == sig


def test_lift_x_even_y_and_off_curve():
    pt = nostr.lift_x(int(PUB, 16))
    assert pt is not None and pt[1] % 2 == 0
    assert nostr.lift_x(nostr.P) is None
    assert nostr.lift_x(int("EEFDEA4CDB677750A420FEE807EACF21EB9898AE79B9768766E4FAA04A2D4A34", 16)) is None


def test_tagged_hash_and_secret_validation():
    import hashlib

    t = hashlib.sha256(b"BIP0340/aux").digest()
    assert nostr.tagged_hash("BIP0340/aux", b"x") == hashlib.sha256(t + t + b"x").digest()
    assert not nostr.is_valid_secret(b"\0" * 32)
    assert not nostr.is_valid_secret(nostr.bytes32(nostr.N))
    assert nostr.is_valid_secret(nostr.bytes32(nostr.N - 1))
    sk = nostr.generate_secret()
    assert len(sk) == 32 and nostr.is_valid_secret(sk)
    with pytest.raises(ValueError):
        nostr.public_key(b"\0" * 32)


def test_known_event_id():
    tags = [["d", "cued"], ["t", "cued-station"]]
    assert nostr.canonical(PUB, 1700000000, 30777, tags, "{}") == '[0,"%s",1700000000,30777,[["d","cued"],["t","cued-station"]],"{}"]' % PUB
    assert nostr.event_id(PUB, 1700000000, 30777, tags, "{}") == "b9b19a4064a063053eaa2e382fdf36ecb94a4e777dc3df01871177d43905e72a"


def test_canonical_keeps_unicode_and_escapes_quotes():
    c = nostr.canonical(PUB, 1, 1, [], 'say "héllo"\n')
    assert c == '[0,"%s",1,1,[],"say \\"héllo\\"\\n"]' % PUB


def test_nip19_npub_nsec_parse():
    assert nostr.npub(PUB) == "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6"
    assert nostr.nip19_parse(nostr.npub(PUB)) == ("npub", PUB)
    assert nostr.nip19_parse(nostr.npub(PUB).upper()) == ("npub", PUB)
    assert nostr.nip19_parse(PUB.upper()) == ("hex", PUB)
    secret = "67dea2ed018072d675f5415ecfaed7d2597555e202d85b3d65ea4e58d2d92ffa"
    ns = nostr.nsec(secret)
    assert ns.startswith("nsec1")
    assert nostr.nip19_parse(ns) == ("nsec", secret)
    assert nostr.bech32_decode("npub1" + "q" * 10) is None       # bad checksum
    assert nostr.bech32_decode("nPub180cvv07tjdrrgpa0j7j7") is None  # mixed case
    assert nostr.nip19_parse("note1" + nostr.npub(PUB)[5:]) is None
    assert nostr.nip19_parse("hello") is None
    hrp, data = nostr.bech32_decode("npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6")
    assert hrp == "npub" and data.hex() == PUB


def test_sign_and_verify_event_round_trip():
    sk = nostr.generate_secret()
    content = json.dumps(nostr.station_state("Sim", "Afterlife", "Evanescence", seq=1), separators=(",", ":"))
    ev = nostr.sign_event(sk.hex(), nostr.STATION_KIND, nostr.STATION_TAGS, content, created_at=1700000000)
    assert ev["pubkey"] == nostr.public_key(sk).hex()
    assert ev["kind"] == 30777 and ev["created_at"] == 1700000000
    assert nostr.event_tag(ev, "d") == "cued"
    assert nostr.verify_event(ev)
    # any change breaks it
    tampered = dict(ev, content=content.replace("Afterlife", "Elsewhere"))
    assert not nostr.verify_event(tampered)
    bad_sig = dict(ev, sig="00" * 64)
    assert not nostr.verify_event(bad_sig)
    assert not nostr.verify_event({"id": "x"})
    # the wire form is a NIP-01 EVENT message that parses back
    verb, rest = nostr.parse_message(nostr.event_message(ev))
    assert verb == "EVENT" and rest[0] == ev
    assert nostr.parse_message("not json") == ("UNKNOWN", ["not json"])
    assert json.loads(nostr.req_message("sub", nostr.station_filter([ev["pubkey"]], since=5))) == [
        "REQ", "sub", {"kinds": [30777], "authors": [ev["pubkey"]], "#d": ["cued"], "since": 5}]
    assert json.loads(nostr.close_message("sub")) == ["CLOSE", "sub"]

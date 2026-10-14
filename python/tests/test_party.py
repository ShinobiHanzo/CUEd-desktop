import hashlib

from cued_companion import nostr, party


def test_canonical_sorted_and_compact():
    a = {"b": 1, "a": "x", "c": [True, 2.5, None]}
    assert party.canonical(a) == '{"a":"x","b":1,"c":[true,2.5,null]}'
    assert party.canonical({"c": 1, "a": 2}) == party.canonical({"a": 2, "c": 1})


def test_digest_matches_desktop_vector():
    # The same envelope the Rust test `digest_matches_phone_vector` hashes.
    env = {"v": 1, "pubkey": "ab" * 32, "ts": 1700000000, "nonce": "0011223344556677", "op": "queue", "arg": {"title": "T", "sha256": "ab"}, "ok": True, "sig": ""}
    body = ("cued-party|1|%s|1700000000|0011223344556677|queue|1|{\"sha256\":\"ab\",\"title\":\"T\"}" % ("ab" * 32)).encode()
    tag = hashlib.sha256(b"CUEd/party").digest()
    assert party.digest(env) == hashlib.sha256(tag + tag + body).digest()


def test_sign_verify_tamper_and_nsec():
    sk = nostr.generate_secret()
    env = party.sign(sk, "search", {"q": "daft punk"})
    assert party.verify(env)
    assert env["pubkey"] == nostr.public_key(sk).hex()
    bad = dict(env, op="skip")
    assert not party.verify(bad)
    bad = dict(env, arg={"q": "other"})
    assert not party.verify(bad)
    assert party.parse_secret(sk.hex()) == sk
    assert party.parse_secret(nostr.nsec(sk.hex())) == sk

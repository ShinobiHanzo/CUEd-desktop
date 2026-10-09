import hashlib
import json

import pytest

from cued_companion import account, nostr

SK = "01" * 32
# Public key of 01*32 (x-only, BIP-340).
PK = "1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f"
# The exact string the genesis block of `create {"name":"Idris"}` at ts 1700000000 hashes:
# [v, height, prev, ts, type, data, pubkey], no whitespace, keys in insertion order.
CANONICAL = '[1,0,"",1700000000,"create",{"name":"Idris"},"%s"]' % PK
# `printf '%s' "$CANONICAL" | sha256sum`
GENESIS_HASH = "72020a8bed2a4e0cf052d376e6ade6fb23f08ee79594b2012e47f248263ebe46"


def test_fixed_genesis_block_hash_and_signature():
    assert nostr.public_key(bytes.fromhex(SK)).hex() == PK
    assert account.canonical(1, 0, "", 1700000000, "create", {"name": "Idris"}, PK) == CANONICAL
    assert hashlib.sha256(CANONICAL.encode()).hexdigest() == GENESIS_HASH
    assert account.block_hash(1, 0, "", 1700000000, "create", {"name": "Idris"}, PK) == GENESIS_HASH
    chain = account.Chain.create(SK, "Idris", 1700000000)
    b = chain.blocks[0]
    assert (b.v, b.height, b.prev, b.ts, b.type, b.data, b.pubkey, b.hash) == (1, 0, "", 1700000000, "create", {"name": "Idris"}, PK, GENESIS_HASH)
    # sig is BIP-340 over the 32-byte hash, by the account key
    assert nostr.verify(bytes.fromhex(GENESIS_HASH), bytes.fromhex(PK), bytes.fromhex(b.sig))
    assert b.verify()
    # the wire shape of protocol.md §8, in that key order
    assert list(b.to_dict().keys()) == ["v", "height", "prev", "ts", "type", "data", "pubkey", "hash", "sig"]
    # with a fixed aux the signature is reproducible too
    s1 = account.sign_block(SK, 0, "", 1700000000, "create", {"name": "Idris"}, aux=b"\0" * 32)
    s2 = account.sign_block(SK, 0, "", 1700000000, "create", {"name": "Idris"}, aux=b"\0" * 32)
    assert s1 == s2 and s1.hash == GENESIS_HASH


def test_data_key_order_is_part_of_the_hash():
    """serde_json keeps insertion order; so must we, and we must not sort."""
    h1 = account.block_hash(1, 1, "p", 2, "bind_device", {"devhash": "d", "name": "Pixel", "kind": "phone"}, PK)
    h2 = account.block_hash(1, 1, "p", 2, "bind_device", {"kind": "phone", "devhash": "d", "name": "Pixel"}, PK)
    assert h1 != h2
    assert account.canonical(1, 1, "p", 2, "settings", {"theme": {"accent": "#fff"}, "relayEnabled": True, "n": None, "f": 1.5, "u": "é"}, PK) == \
        '[1,1,"p",2,"settings",{"theme":{"accent":"#fff"},"relayEnabled":true,"n":null,"f":1.5,"u":"é"},"%s"]' % PK
    # a chain round-tripped through JSON text keeps its order and still validates
    chain = account.Chain.create(SK, "Idris", 1)
    chain.append(SK, "bind_device", {"devhash": account.devhash("0011223344556677", "salt"), "name": "Pixel 8", "kind": "phone"}, 2)
    back = account.Chain.from_json(chain.to_json())
    back.validate()
    assert back.to_dict() == chain.to_dict()
    assert json.loads(chain.to_json())["blocks"][1]["data"] == {"devhash": account.devhash("0011223344556677", "salt"), "name": "Pixel 8", "kind": "phone"}


def test_devhash_and_account_id():
    assert account.devhash("0011223344556677", "salt") == hashlib.sha256(b"cued-device|0011223344556677|salt").hexdigest()
    assert account.devhash("0011223344556677", "salt") == "e5f909d260fb6a1faefd782ed5d8ec1b5bd138907b29ef1568f453eac49217c4"
    assert account.account_id(PK) == hashlib.sha256(bytes.fromhex(PK)).hexdigest() == "ec3925bdbd24aa1cfd002f9ccf52f2bc30061721d8841e86c93c087c1fd2bdcb"
    assert account.account_id("zz") == hashlib.sha256(b"").hexdigest()  # like hex::decode(..).unwrap_or_default()
    assert len(account.new_salt()) == 32


def test_build_validate_merge_like_account_rs():
    sk = nostr.generate_secret().hex()
    a = account.Chain.create(sk, "Idris", 1)
    dh = account.devhash("0011223344556677", "salt")
    a.append(sk, "bind_device", {"devhash": dh, "name": "Pixel", "kind": "phone"}, 2)
    a.validate()
    assert a.is_valid()
    assert a.name() == "Idris"
    assert a.devices() == [{"devhash": dh, "name": "Pixel", "kind": "phone", "boundAt": 2}]
    assert a.height == 2 and a.tip.height == 1 and a.tip_hash == a.blocks[1].hash and a.pubkey == a.blocks[0].pubkey
    for b in a.blocks:
        assert b.verify() and nostr.verify(bytes.fromhex(b.hash), bytes.fromhex(b.pubkey), bytes.fromhex(b.sig))

    b = account.Chain.from_dict(a.to_dict())
    b.append(sk, "settings", {"relayEnabled": True}, 3)
    b.append(sk, "settings", {"theme": {"accent": "#fff"}}, 4)
    assert b.settings() == {"relayEnabled": True, "theme": {"accent": "#fff"}}
    assert b.latest("settings") == {"theme": {"accent": "#fff"}}
    assert b.latest("profile") is None
    assert a.merge(b) is True
    assert a.height == 4
    assert a.merge(b) is False
    # A shorter copy never wins.
    short = account.Chain(a.blocks[:2])
    assert a.merge(short) is False and a.height == 4
    # Equal height, different tip: the lower tip hash wins.
    c1 = account.Chain.from_dict(short.to_dict())
    c2 = account.Chain.from_dict(short.to_dict())
    c1.append(sk, "profile", {"name": "One"}, 5)
    c2.append(sk, "profile", {"name": "Two"}, 5)
    lo, hi = sorted([c1, c2], key=lambda c: c.tip_hash)
    assert hi.merge(lo) is True and hi.tip_hash == lo.tip_hash
    assert lo.merge(hi) is False
    # An empty chain takes whatever valid copy it is given.
    empty = account.Chain()
    assert empty.merge(a) is True and empty.height == 4
    # Tampering breaks validation (hash no longer matches), and so does a re-hashed block (signature no longer matches).
    bad = account.Chain.from_dict(a.to_dict())
    bad.blocks[1].data["name"] = "Other"
    with pytest.raises(account.ChainError, match="block 1"):
        bad.validate()
    bad.blocks[1].hash = bad.blocks[1].compute_hash()
    assert not bad.is_valid()
    with pytest.raises(account.ChainError):
        a.merge(bad)
    # Another key cannot extend it, and a foreign chain cannot merge.
    other = nostr.generate_secret().hex()
    with pytest.raises(account.ChainError, match="only the account key"):
        a.append(other, "profile", {"name": "x"}, 5)
    foreign = account.Chain.create(other, "x", 1)
    with pytest.raises(account.ChainError, match="different account"):
        a.merge(foreign)
    # profile renames, unbind empties devices
    a.append(sk, "profile", {"name": "Idris R."}, 6)
    assert a.name() == "Idris R."
    a.append(sk, "unbind_device", {"devhash": dh}, 7)
    assert a.devices() == []
    a.validate()


def test_validate_rejects_bad_shapes():
    sk = nostr.generate_secret().hex()
    with pytest.raises(account.ChainError, match="empty"):
        account.Chain().validate()
    c = account.Chain.create(sk, "n", 1)
    c.append(sk, "profile", {"name": "m"}, 2)
    # a chain that does not start with create
    with pytest.raises(account.ChainError, match="block 0"):
        account.Chain(c.blocks[1:]).validate()
    # wrong height numbering / broken prev link
    d = account.Chain.from_dict(c.to_dict())
    d.blocks[1].height = 2
    assert not d.is_valid()
    d = account.Chain.from_dict(c.to_dict())
    d.blocks[1].prev = "00" * 32
    assert not d.is_valid()
    # v must be 1
    d = account.Chain.from_dict(c.to_dict())
    d.blocks[0].v = 2
    assert not d.blocks[0].verify()
    # malformed JSON shapes
    with pytest.raises(account.ChainError):
        account.Chain.from_dict({"blocks": [{"v": 1}]})
    with pytest.raises(account.ChainError):
        account.Chain.from_dict({"nope": 1})
    with pytest.raises(account.ChainError):
        account.Chain.from_json("not json")
    with pytest.raises(account.ChainError):
        account.Block.from_dict(dict(c.blocks[0].to_dict(), data="string"))
    # a bare list of blocks is accepted too
    assert account.Chain.from_dict(c.to_dict()["blocks"]).to_dict() == c.to_dict()

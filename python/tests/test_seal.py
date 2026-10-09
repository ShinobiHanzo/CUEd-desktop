import base64

import pytest

from cued_companion import seal

SECRET = bytes.fromhex("0f" * 16)


def test_known_vector_shared_with_rust_and_kotlin():
    """The same numbers as seal.rs ``known_vector`` and the phone's SealTest."""
    ke, km = seal.keys(SECRET, "account")
    assert ke.hex() == "5ea07cd5ce6b821640f080ee725490f4958820c70956cbbaf2bbb850c3b2956d"
    nonce = b"\0" * 16
    ct = seal.xor_keystream(ke, nonce, b"hello")
    assert ct.hex() == "e19510564f"
    import hashlib
    import hmac

    assert hmac.new(km, nonce + ct, hashlib.sha256).hexdigest() == "30e13d3793e94f365e8cb8b3e9fee951adac2972f37a17413bc1e8ea0e1b216b"
    sealed = seal.seal_with_nonce(SECRET, "account", b"hello", nonce)
    raw = base64.urlsafe_b64decode(sealed + "=" * (-len(sealed) % 4))
    assert raw == nonce + ct + bytes.fromhex("30e13d3793e94f365e8cb8b3e9fee951adac2972f37a17413bc1e8ea0e1b216b")
    assert "=" not in sealed and "+" not in sealed and "/" not in sealed
    assert seal.open_sealed(SECRET, "account", sealed) == b"hello"
    # padded base64 and surrounding whitespace are tolerated on the way in
    assert seal.open_sealed(SECRET, "account", " " + sealed + "==\n") == b"hello"


def test_round_trip_multi_block_and_empty():
    msg = b'{"secret_hex":"..."}' * 7  # spans several 32-byte keystream blocks
    s = seal.seal(SECRET, "account", msg)
    assert seal.open_sealed(SECRET, "account", s) == msg
    assert seal.seal(SECRET, "account", msg) != s  # fresh nonce every time
    assert seal.open_sealed(SECRET, "x", seal.seal(SECRET, "x", b"")) == b""
    assert len(base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))) == 16 + len(msg) + 32


def test_open_rejects_wrong_secret_label_and_tampering():
    s = seal.seal(SECRET, "account", b"hello world, this is longer than one block of keystream")
    with pytest.raises(seal.SealError):
        seal.open_sealed(bytes.fromhex("00" * 16), "account", s)
    with pytest.raises(seal.SealError):
        seal.open_sealed(SECRET, "other", s)
    raw = bytearray(base64.urlsafe_b64decode(s + "=" * (-len(s) % 4)))
    for i in (0, 20, len(raw) - 1):  # nonce, ciphertext, tag
        bad = bytearray(raw)
        bad[i] ^= 1
        with pytest.raises(seal.SealError):
            seal.open_sealed(SECRET, "account", base64.urlsafe_b64encode(bytes(bad)).decode().rstrip("="))
    with pytest.raises(seal.SealError):
        seal.open_sealed(SECRET, "account", "zz")       # too short
    with pytest.raises(seal.SealError):
        seal.open_sealed(SECRET, "account", "!!!!")     # not base64url
    with pytest.raises(seal.SealError):
        seal.open_sealed(SECRET, "account", base64.urlsafe_b64encode(b"\0" * 47).decode())  # one byte short of nonce + tag

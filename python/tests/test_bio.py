import hashlib

import pytest

from cued_companion import bio

# RFC 6979 A.2.5: ECDSA P-256 with SHA-256, deterministic nonces.
RFC_PRIV = "C9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721"
RFC_PUB = "04" + "60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6" + "7903FE1008B8BC99A41AE9E95628BC64F2F1B20C2D7E9F5177A3C294D4462299"
RFC_VECTORS = {
    b"sample": ("A6E3C57DD01ABE90086538398355DD4C3B17AA873382B0F24D6129493D8AAD60",
                "EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716",
                "F7CB1C942D657C41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8"),
    b"test": ("D16B6AE827F17175E040871A1C7EC3500192C4C92677336EC2537ACAEE0008E0",
              "F1ABB023518351CD71D881567B1EA663ED3EFCF6C5132B354F28D3B0B7D38367",
              "019F4113742A2B14BD25926B49C649155F267E60D3814B4C0CC84250E46F0083"),
}


def test_curve_and_known_public_key():
    assert bio.on_curve((bio.GX, bio.GY))
    assert bio.scalar_mult(bio.N, (bio.GX, bio.GY)) is None  # n * G = infinity
    assert bio.encode_point(bio.public_point(RFC_PRIV)) == RFC_PUB.lower()
    assert bio.decode_point(RFC_PUB) == bio.public_point(RFC_PRIV)
    # compressed form decodes to the same point
    x, y = bio.public_point(RFC_PRIV)
    comp = ("03" if y & 1 else "02") + x.to_bytes(32, "big").hex()
    assert bio.decode_point(comp) == (x, y)
    assert bio.valid_biokey(RFC_PUB) and not bio.valid_biokey("abcd") and not bio.valid_biokey("04" + "00" * 64)
    assert bio.decode_point("zz") is None


@pytest.mark.parametrize("msg", sorted(RFC_VECTORS))
def test_rfc6979_vectors(msg):
    k, r, s = RFC_VECTORS[msg]
    digest = hashlib.sha256(msg).digest()
    assert bio.rfc6979_nonce(int(RFC_PRIV, 16), digest) == int(k, 16)
    sig = bio.sign(RFC_PRIV, msg)
    assert sig == (r + s).lower()
    assert bio.verify(RFC_PUB, msg, sig)
    assert bio.verify(RFC_PUB, msg, bio.rs_to_der(sig).hex())   # the Keystore's DER form
    assert not bio.verify(RFC_PUB, msg + b"x", sig)
    assert not bio.verify(RFC_PUB, msg, sig[:-2] + "00")
    assert not bio.verify(RFC_PUB, msg, "00" * 64)


def test_generate_sign_verify_and_assertion():
    priv, pub = bio.generate()
    assert len(priv) == 64 and len(pub) == 130 and pub.startswith("04")
    device, challenge = "0011223344556677", "ab" * 16
    a = bio.assertion(priv, device, challenge)
    ch, sig = a.split(".")
    assert ch == challenge and len(sig) == 128
    assert bio.message(device, challenge) == b"cued-bio|0011223344556677|" + challenge.encode()
    assert bio.verify(pub, bio.message(device, challenge), sig)
    assert bio.verify_assertion(pub, device, a)
    assert not bio.verify_assertion(pub, "0011223344556678", a)   # another device
    assert not bio.verify_assertion(pub, device, challenge + "." + "11" * 64)
    assert not bio.verify_assertion(pub, device, "nodot")
    other = bio.generate()[1]
    assert not bio.verify_assertion(other, device, a)
    # bad keys and signature encodings are refused, not raised
    assert not bio.verify("04" + "11" * 64, b"x", sig)
    assert not bio.verify(pub, b"x", "zz")
    assert bio.der_to_rs(b"\x02\x01\x00") is None and bio.der_to_rs(b"\x30\x02\x05\x00") is None
    with pytest.raises(ValueError):
        bio.sign("00" * 32, b"x")


def test_against_cryptography_when_installed():
    """Sign with ours, verify with theirs and the other way round. Tests only; the runtime stays stdlib."""
    ec = pytest.importorskip("cryptography.hazmat.primitives.asymmetric.ec")
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

    priv, pub = bio.generate()
    theirs = ec.derive_private_key(int(priv, 16), ec.SECP256R1())
    their_pub = theirs.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
    assert their_pub.hex() == pub
    for msg in (b"cued-bio|0011223344556677|" + b"ab" * 16, b"", b"x" * 1000):
        ours = bio.sign(priv, msg)
        theirs.public_key().verify(bio.rs_to_der(ours), msg, ec.ECDSA(hashes.SHA256()))
        der = theirs.sign(msg, ec.ECDSA(hashes.SHA256()))
        r, s = decode_dss_signature(der)
        assert bio.verify(pub, msg, r.to_bytes(32, "big").hex() + s.to_bytes(32, "big").hex())
        assert bio.verify(pub, msg, der.hex())
        assert bio.der_to_rs(der) == (r, s)

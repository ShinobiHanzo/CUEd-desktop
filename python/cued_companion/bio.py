"""
Biometric assertions (``docs/protocol.md`` §7) with a pure-Python ECDSA P-256.

On a real phone the key lives in the Android Keystore and only signs after a
biometric prompt; the simulator stands in for that with an ordinary P-256 key
held in its state file. The desktop (``crates/cued-proto/src/bio.rs``)
verifies the 64-byte ``r||s`` form over SHA-256 of
``cued-bio|<device>|<challenge>``; the DER form is also accepted there but is
not produced here.

Plain integers, Jacobian coordinates, RFC 6979 deterministic nonces. Not
constant-time; one signature every ten minutes is the whole workload.
"""
from __future__ import annotations

import hashlib
import hmac
import os
from typing import Optional, Tuple

# NIST P-256 (secp256r1), FIPS 186-4 D.1.2.3
P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
A = P - 3
B = 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B
N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551
GX = 0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296
GY = 0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5

_J = Tuple[int, int, int]  # Jacobian (x, y, z); z == 0 is infinity
_INF: _J = (0, 1, 0)
_G: _J = (GX, GY, 1)

Point = Tuple[int, int]


def _dbl(p: _J) -> _J:
    x, y, z = p
    if z == 0 or y == 0:
        return _INF
    # a = -3 shortcut: m = 3 (x - z^2)(x + z^2)
    zz = z * z % P
    ysq = y * y % P
    s = 4 * x * ysq % P
    m = 3 * (x - zz) * (x + zz) % P
    nx = (m * m - 2 * s) % P
    ny = (m * (s - nx) - 8 * ysq * ysq) % P
    nz = 2 * y * z % P
    return (nx, ny, nz)


def _add(p: _J, q: _J) -> _J:
    if p[2] == 0:
        return q
    if q[2] == 0:
        return p
    x1, y1, z1 = p
    x2, y2, z2 = q
    z1z1 = z1 * z1 % P
    z2z2 = z2 * z2 % P
    u1 = x1 * z2z2 % P
    u2 = x2 * z1z1 % P
    s1 = y1 * z2 * z2z2 % P
    s2 = y2 * z1 * z1z1 % P
    if u1 == u2:
        return _INF if s1 != s2 else _dbl(p)
    h = (u2 - u1) % P
    r = (s2 - s1) % P
    h2 = h * h % P
    h3 = h2 * h % P
    u1h2 = u1 * h2 % P
    nx = (r * r - h3 - 2 * u1h2) % P
    ny = (r * (u1h2 - nx) - s1 * h3) % P
    nz = h * z1 * z2 % P
    return (nx, ny, nz)


def _mul(p: _J, k: int) -> _J:
    r = _INF
    a = p
    kk = k % N
    while kk > 0:
        if kk & 1:
            r = _add(r, a)
        a = _dbl(a)
        kk >>= 1
    return r


def _affine(p: _J) -> Optional[Point]:
    x, y, z = p
    if z == 0:
        return None
    zi = pow(z, P - 2, P)
    zi2 = zi * zi % P
    return (x * zi2 % P, y * zi2 * zi % P)


def on_curve(pt: Point) -> bool:
    x, y = pt
    return 0 <= x < P and 0 <= y < P and (y * y - (x * x * x + A * x + B)) % P == 0


def scalar_mult(k: int, pt: Point) -> Optional[Point]:
    return _affine(_mul((pt[0], pt[1], 1), k))


# ---------------------------------------------------------------- keys


def is_valid_secret(d: int) -> bool:
    return 0 < d < N


def public_point(priv_hex: str) -> Point:
    d = int(priv_hex, 16)
    if not is_valid_secret(d):
        raise ValueError("invalid P-256 secret")
    pt = _affine(_mul(_G, d))
    if pt is None:
        raise ValueError("infinity")
    return pt


def encode_point(pt: Point) -> str:
    """65-byte uncompressed SEC1 (``04 || x || y``), hex."""
    return "04" + pt[0].to_bytes(32, "big").hex() + pt[1].to_bytes(32, "big").hex()


def decode_point(pub_hex: str) -> Optional[Point]:
    """Accepts uncompressed (65 bytes) and compressed (33 bytes) SEC1; None when not on the curve."""
    try:
        raw = bytes.fromhex(pub_hex.strip())
    except ValueError:
        return None
    if len(raw) == 65 and raw[0] == 4:
        pt = (int.from_bytes(raw[1:33], "big"), int.from_bytes(raw[33:], "big"))
        return pt if on_curve(pt) else None
    if len(raw) == 33 and raw[0] in (2, 3):
        x = int.from_bytes(raw[1:], "big")
        if x >= P:
            return None
        rhs = (pow(x, 3, P) + A * x + B) % P
        y = pow(rhs, (P + 1) >> 2, P)
        if y * y % P != rhs:
            return None
        if (y & 1) != (raw[0] & 1):
            y = P - y
        return (x, y)
    return None


def generate() -> Tuple[str, str]:
    """(private scalar hex, 65-byte uncompressed public key hex): the sim's stand-in for the Keystore key."""
    while True:
        d = int.from_bytes(os.urandom(32), "big")
        if is_valid_secret(d):
            break
    priv = d.to_bytes(32, "big").hex()
    return priv, encode_point(public_point(priv))


def valid_biokey(pub_hex: str) -> bool:
    return decode_point(pub_hex) is not None


# ---------------------------------------------------------------- RFC 6979 + ECDSA


def _bits2int(b: bytes) -> int:
    v = int.from_bytes(b, "big")
    excess = len(b) * 8 - 256
    return v >> excess if excess > 0 else v


def rfc6979_nonce(d: int, h1: bytes) -> int:
    """Deterministic k (RFC 6979 §3.2) with HMAC-SHA256; ``h1`` is the message hash."""
    x = d.to_bytes(32, "big")
    h = (_bits2int(h1) % N).to_bytes(32, "big")
    v = b"\x01" * 32
    k = b"\x00" * 32
    k = hmac.new(k, v + b"\x00" + x + h, hashlib.sha256).digest()
    v = hmac.new(k, v, hashlib.sha256).digest()
    k = hmac.new(k, v + b"\x01" + x + h, hashlib.sha256).digest()
    v = hmac.new(k, v, hashlib.sha256).digest()
    while True:
        v = hmac.new(k, v, hashlib.sha256).digest()
        cand = _bits2int(v)
        if 0 < cand < N:
            return cand
        k = hmac.new(k, v + b"\x00", hashlib.sha256).digest()
        v = hmac.new(k, v, hashlib.sha256).digest()


def sign_digest(priv_hex: str, digest: bytes, k: Optional[int] = None) -> str:
    """ECDSA over an already-hashed message; 64-byte ``r||s`` hex (``s`` is not normalised: verifiers accept either)."""
    d = int(priv_hex, 16)
    if not is_valid_secret(d):
        raise ValueError("invalid P-256 secret")
    z = _bits2int(digest) % N
    while True:
        kk = rfc6979_nonce(d, digest) if k is None else k
        r_pt = _affine(_mul(_G, kk))
        if r_pt is None:
            raise ValueError("infinity")
        r = r_pt[0] % N
        s = pow(kk, N - 2, N) * (z + r * d) % N
        if r != 0 and s != 0:
            return r.to_bytes(32, "big").hex() + s.to_bytes(32, "big").hex()
        if k is not None:
            raise ValueError("bad nonce")
        digest = hashlib.sha256(digest).digest()  # astronomically unlikely; re-derive


def sign(priv_hex: str, message: bytes) -> str:
    """ECDSA P-256 over SHA-256(message), 64-byte ``r||s`` hex."""
    return sign_digest(priv_hex, hashlib.sha256(message).digest())


def verify_digest(pub_hex: str, digest: bytes, sig_hex: str) -> bool:
    pt = decode_point(pub_hex)
    if pt is None:
        return False
    try:
        sig = bytes.fromhex(sig_hex.strip())
    except ValueError:
        return False
    if len(sig) == 64:
        r, s = int.from_bytes(sig[:32], "big"), int.from_bytes(sig[32:], "big")
    else:
        rs = der_to_rs(sig)
        if rs is None:
            return False
        r, s = rs
    if not (0 < r < N and 0 < s < N):
        return False
    z = _bits2int(digest) % N
    w = pow(s, N - 2, N)
    u1 = z * w % N
    u2 = r * w % N
    x = _affine(_add(_mul(_G, u1), _mul((pt[0], pt[1], 1), u2)))
    return x is not None and x[0] % N == r


def verify(pub_hex: str, message: bytes, sig_hex: str) -> bool:
    """True when ``sig_hex`` (64-byte ``r||s`` or DER) signs SHA-256(message) under ``pub_hex``."""
    return verify_digest(pub_hex, hashlib.sha256(message).digest(), sig_hex)


def der_to_rs(der: bytes) -> Optional[Tuple[int, int]]:
    """Minimal DER ``SEQUENCE { INTEGER r, INTEGER s }`` parser (what the Keystore hands out)."""
    try:
        if der[0] != 0x30:
            return None
        i = 2 if der[1] < 0x80 else 2 + (der[1] & 0x7F)
        out = []
        for _ in range(2):
            if der[i] != 0x02:
                return None
            ln = der[i + 1]
            out.append(int.from_bytes(der[i + 2 : i + 2 + ln], "big"))
            i += 2 + ln
        return out[0], out[1]
    except IndexError:
        return None


def rs_to_der(sig_hex: str) -> bytes:
    """The DER form of a 64-byte ``r||s`` signature (the Keystore's native output), for tests."""
    raw = bytes.fromhex(sig_hex)
    out = b""
    for v in (int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big")):
        b = v.to_bytes((v.bit_length() + 8) // 8 or 1, "big")  # leading 0x00 when the top bit is set
        out += b"\x02" + bytes([len(b)]) + b
    return b"\x30" + bytes([len(out)]) + out


# ---------------------------------------------------------------- the assertion itself


def message(device: str, challenge: str) -> bytes:
    """What the phone signs: ``cued-bio|<device>|<challenge>``."""
    return ("cued-bio|%s|%s" % (device, challenge)).encode("ascii")


def assertion(priv_hex: str, device: str, challenge: str) -> str:
    """``X-Cued-Assertion`` header value: ``<challenge>.<sig hex>``."""
    return "%s.%s" % (challenge, sign(priv_hex, message(device, challenge)))


def verify_assertion(pub_hex: str, device: str, header: str) -> bool:
    """What the desktop does with the header (``bio.rs``): split, rebuild the message, verify."""
    if "." not in header:
        return False
    challenge, sig = header.split(".", 1)
    return verify(pub_hex, message(device, challenge), sig)

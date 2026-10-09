"""
secp256k1 + BIP-340 Schnorr, NIP-01 events and NIP-19 bech32, with no
dependencies.

A line-for-line port of the phone's ``Secp256k1.kt``, ``Bech32.kt`` and
``Nostr.kt`` onto Python integers. A station identity signs a few hundred
bytes per track change, so speed does not matter and every step can be read
and checked against the BIP-340 vectors. Not constant-time.
"""
from __future__ import annotations

import hashlib
import json
import os
import time
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
GX = 0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
GY = 0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8

# Jacobian (x, y, z); z == 0 is the point at infinity.
_J = Tuple[int, int, int]
_INF: _J = (0, 1, 0)
_G: _J = (GX, GY, 1)

Point = Tuple[int, int]  # affine


def _dbl(p: _J) -> _J:
    x, y, z = p
    if z == 0 or y == 0:
        return _INF
    ysq = y * y % P
    s = 4 * x * ysq % P
    m = 3 * x * x % P
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


def _jac(p: Point) -> _J:
    return (p[0], p[1], 1)


def has_even_y(p: Point) -> bool:
    return p[1] & 1 == 0


def lift_x(x: int) -> Optional[Point]:
    """The point with this x and an even y, or None if x is not on the curve."""
    if x < 0 or x >= P:
        return None
    c = (pow(x, 3, P) + 7) % P
    y = pow(c, (P + 1) >> 2, P)
    if y * y % P != c:
        return None
    return (x, P - y if y & 1 else y)


def bytes32(v: int) -> bytes:
    return (v % (1 << 256)).to_bytes(32, "big")


def to_int(b: bytes) -> int:
    return int.from_bytes(b, "big")


def sha256(*parts: bytes) -> bytes:
    h = hashlib.sha256()
    for p in parts:
        h.update(p)
    return h.digest()


def tagged_hash(tag: str, *parts: bytes) -> bytes:
    t = sha256(tag.encode("utf-8"))
    return sha256(t, t, *parts)


def is_valid_secret(sk: bytes) -> bool:
    return len(sk) == 32 and 0 < to_int(sk) < N


def generate_secret() -> bytes:
    while True:
        b = os.urandom(32)
        if is_valid_secret(b):
            return b


def public_key(sk: bytes) -> bytes:
    """x-only public key, 32 bytes."""
    if not is_valid_secret(sk):
        raise ValueError("invalid secret key")
    p = _affine(_mul(_G, to_int(sk)))
    if p is None:
        raise ValueError("infinity")
    return bytes32(p[0])


def _xor(a: bytes, b: bytes) -> bytes:
    return bytes(x ^ y for x, y in zip(a, b))


def sign(msg: bytes, sk: bytes, aux: Optional[bytes] = None) -> bytes:
    """BIP-340 signature (64 bytes); the message may be any length (Nostr signs the 32-byte id)."""
    if aux is None:
        aux = os.urandom(32)
    if len(aux) != 32:
        raise ValueError("aux must be 32 bytes")
    if not is_valid_secret(sk):
        raise ValueError("invalid secret key")
    d0 = to_int(sk)
    pt = _affine(_mul(_G, d0))
    if pt is None:
        raise ValueError("infinity")
    d = d0 if has_even_y(pt) else N - d0
    t = _xor(bytes32(d), tagged_hash("BIP0340/aux", aux))
    px = bytes32(pt[0])
    rand = tagged_hash("BIP0340/nonce", t, px, msg)
    k0 = to_int(rand) % N
    if k0 == 0:
        raise ValueError("bad nonce")
    r = _affine(_mul(_G, k0))
    if r is None:
        raise ValueError("infinity")
    k = k0 if has_even_y(r) else N - k0
    rx = bytes32(r[0])
    e = to_int(tagged_hash("BIP0340/challenge", rx, px, msg)) % N
    s = (k + e * d) % N
    sig = rx + bytes32(s)
    if not verify(msg, px, sig):
        raise ValueError("signature self-check failed")
    return sig


def verify(msg: bytes, pub: bytes, sig: bytes) -> bool:
    if len(pub) != 32 or len(sig) != 64:
        return False
    pt = lift_x(to_int(pub))
    if pt is None:
        return False
    r = to_int(sig[:32])
    s = to_int(sig[32:])
    if r >= P or s >= N:
        return False
    e = to_int(tagged_hash("BIP0340/challenge", sig[:32], pub, msg)) % N
    rr = _affine(_add(_mul(_G, s), _mul(_jac(pt), N - e)))
    if rr is None:
        return False
    return has_even_y(rr) and rr[0] == r


# ---------------------------------------------------------------- bech32 (BIP-173) / NIP-19

_CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
_GEN = (0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3)


def _polymod(values: Iterable[int]) -> int:
    chk = 1
    for v in values:
        b = chk >> 25
        chk = ((chk & 0x1FFFFFF) << 5) ^ v
        for i in range(5):
            if (b >> i) & 1:
                chk ^= _GEN[i]
    return chk


def _hrp_expand(hrp: str) -> List[int]:
    return [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]


def _convert_bits(data: Sequence[int], frm: int, to: int, pad: bool) -> List[int]:
    acc = 0
    bits = 0
    out: List[int] = []
    maxv = (1 << to) - 1
    for b in data:
        acc = (acc << frm) | (b & 0xFF)
        bits += frm
        while bits >= to:
            bits -= to
            out.append((acc >> bits) & maxv)
    if pad:
        if bits > 0:
            out.append((acc << (to - bits)) & maxv)
    elif bits >= frm or ((acc << (to - bits)) & maxv) != 0:
        raise ValueError("invalid padding")
    return out


def bech32_encode(hrp: str, data: bytes) -> str:
    d = _convert_bits(data, 8, 5, True)
    values = _hrp_expand(hrp) + d + [0] * 6
    mod = _polymod(values) ^ 1
    checksum = [(mod >> (5 * (5 - i))) & 31 for i in range(6)]
    return hrp + "1" + "".join(_CHARSET[c] for c in d + checksum)


def bech32_decode(text: str) -> Optional[Tuple[str, bytes]]:
    """(hrp, payload bytes), or None when the string is not valid bech32."""
    s = text.strip()
    if s != s.lower() and s != s.upper():
        return None
    t = s.lower()
    pos = t.rfind("1")
    if pos < 1 or pos + 7 > len(t):
        return None
    hrp = t[:pos]
    data: List[int] = []
    for ch in t[pos + 1 :]:
        c = _CHARSET.find(ch)
        if c < 0:
            return None
        data.append(c)
    if _polymod(_hrp_expand(hrp) + data) != 1:
        return None
    try:
        payload = _convert_bits(data[:-6], 5, 8, False)
    except ValueError:
        return None
    return hrp, bytes(payload)


def npub(pubkey_hex: str) -> str:
    return bech32_encode("npub", bytes.fromhex(pubkey_hex))


def nsec(secret_hex: str) -> str:
    return bech32_encode("nsec", bytes.fromhex(secret_hex))


def nip19_parse(text: str) -> Optional[Tuple[str, str]]:
    """Accepts npub/nsec or bare 64-char hex; returns (kind, hex) with kind in npub/nsec/hex."""
    t = text.strip()
    if len(t) == 64:
        try:
            bytes.fromhex(t)
            return "hex", t.lower()
        except ValueError:
            pass
    decoded = bech32_decode(t)
    if decoded is None:
        return None
    hrp, data = decoded
    if len(data) != 32 or hrp not in ("npub", "nsec"):
        return None
    return hrp, data.hex()


# ---------------------------------------------------------------- NIP-01

STATION_KIND = 30777
STATION_D_TAG = "cued"
STATION_TAGS: List[List[str]] = [["d", STATION_D_TAG], ["t", "cued-station"]]

Tags = List[List[str]]


def _dumps(obj) -> str:
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False)


def canonical(pubkey: str, created_at: int, kind: int, tags: Tags, content: str) -> str:
    """NIP-01 form hashed for the id: ``[0, pubkey, created_at, kind, tags, content]``."""
    return _dumps([0, pubkey, created_at, kind, tags, content])


def event_id(pubkey: str, created_at: int, kind: int, tags: Tags, content: str) -> str:
    return hashlib.sha256(canonical(pubkey, created_at, kind, tags, content).encode("utf-8")).hexdigest()


def sign_event(secret_hex: str, kind: int, tags: Tags, content: str, created_at: Optional[int] = None) -> Dict:
    if created_at is None:
        created_at = int(time.time())
    sk = bytes.fromhex(secret_hex)
    pub = public_key(sk).hex()
    eid = event_id(pub, created_at, kind, tags, content)
    sig = sign(bytes.fromhex(eid), sk).hex()
    return {"id": eid, "pubkey": pub, "created_at": created_at, "kind": kind, "tags": tags, "content": content, "sig": sig}


def verify_event(e: Dict) -> bool:
    try:
        tags = [[str(x) for x in t] for t in e["tags"]]
        if e["id"] != event_id(e["pubkey"], int(e["created_at"]), int(e["kind"]), tags, e["content"]):
            return False
        return verify(bytes.fromhex(e["id"]), bytes.fromhex(e["pubkey"]), bytes.fromhex(e["sig"]))
    except (KeyError, TypeError, ValueError):
        return False


def event_tag(e: Dict, name: str) -> Optional[str]:
    for t in e.get("tags", []):
        if len(t) >= 2 and t[0] == name:
            return t[1]
    return None


def event_message(e: Dict) -> str:
    return _dumps(["EVENT", e])


def req_message(sub_id: str, *filters: Dict) -> str:
    return _dumps(["REQ", sub_id, *filters])


def close_message(sub_id: str) -> str:
    return _dumps(["CLOSE", sub_id])


def parse_message(text: str) -> Tuple[str, list]:
    """(verb, rest) for EVENT/EOSE/OK/CLOSED/NOTICE; ("UNKNOWN", [text]) otherwise."""
    try:
        arr = json.loads(text)
        if isinstance(arr, list) and arr and isinstance(arr[0], str):
            return arr[0], arr[1:]
    except (ValueError, TypeError):
        pass
    return "UNKNOWN", [text]


def station_filter(authors: Sequence[str], since: Optional[int] = None) -> Dict:
    f: Dict = {"kinds": [STATION_KIND], "authors": list(authors), "#d": [STATION_D_TAG]}
    if since is not None:
        f["since"] = since
    return f


def station_state(name: str, title: str, artist: str, *, album: str = "", duration_ms: int = 0, link: Optional[str] = None,
                  key: str = "", status: str = "on_air", started_at_ms: Optional[int] = None, position_ms: int = 0,
                  seq: int = 0, next_tracks: Optional[list] = None) -> Dict:
    """A ``StationState`` as the phone serialises it (all defaults written out)."""
    if started_at_ms is None:
        started_at_ms = int(time.time() * 1000)
    return {
        "v": 1,
        "name": name,
        "status": status,
        "now": {"title": title, "artist": artist, "album": album, "durationMs": duration_ms, "link": link, "cover": None, "key": key},
        "startedAt": started_at_ms,
        "positionMs": position_ms,
        "next": next_tracks or [],
        "seq": seq,
    }

"""
``seal`` / ``open_sealed`` from ``docs/protocol.md`` §8: HMAC-SHA256 in counter
mode with an encrypt-then-MAC tag, keyed from a secret two devices only ever
saw in a QR code. Mirrors ``crates/cued-proto/src/seal.rs`` byte for byte.

    k   = HMAC-SHA256(key = "cued-seal-v1", msg = secret || label)
    ke  = HMAC-SHA256(k, "enc");  km = HMAC-SHA256(k, "mac")
    nonce = 16 random bytes
    keystream block i = HMAC-SHA256(ke, nonce || i as u32 big-endian)
    ct  = plaintext XOR keystream
    tag = HMAC-SHA256(km, nonce || ct)
    sealed = base64url(nonce || ct || tag)      (no padding)
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import os
from typing import Tuple

NONCE_LEN = 16
TAG_LEN = 32
LABEL_ACCOUNT = "account"


class SealError(ValueError):
    """Wrong secret, wrong label, or a tampered byte."""


def _hmac(key: bytes, msg: bytes) -> bytes:
    return hmac.new(key, msg, hashlib.sha256).digest()


def keys(secret: bytes, label: str) -> Tuple[bytes, bytes]:
    """(ke, km) for this secret and label."""
    k = _hmac(b"cued-seal-v1", bytes(secret) + label.encode("utf-8"))
    return _hmac(k, b"enc"), _hmac(k, b"mac")


def xor_keystream(ke: bytes, nonce: bytes, data: bytes) -> bytes:
    if len(nonce) != NONCE_LEN:
        raise ValueError("nonce must be 16 bytes")
    out = bytearray(len(data))
    for i in range(0, len(data), 32):
        block = _hmac(ke, nonce + (i // 32).to_bytes(4, "big"))
        chunk = data[i : i + 32]
        out[i : i + len(chunk)] = bytes(a ^ b for a, b in zip(chunk, block))
    return bytes(out)


def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def _unb64url(text: str) -> bytes:
    t = text.strip()
    pad = "=" * (-len(t) % 4)
    try:
        return base64.urlsafe_b64decode(t + pad)
    except (ValueError, TypeError):
        raise SealError("not base64url")


def seal_with_nonce(secret: bytes, label: str, plaintext: bytes, nonce: bytes) -> str:
    """Deterministic form of :func:`seal`, for tests and fixed vectors."""
    ke, km = keys(secret, label)
    ct = xor_keystream(ke, nonce, plaintext)
    body = nonce + ct
    return _b64url(body + _hmac(km, body))


def seal(secret: bytes, label: str, plaintext: bytes) -> str:
    """Seals ``plaintext`` under ``secret`` (raw bytes, e.g. the decoded pairing secret) and ``label``."""
    return seal_with_nonce(secret, label, plaintext, os.urandom(NONCE_LEN))


def open_sealed(secret: bytes, label: str, sealed: str) -> bytes:
    """Opens what :func:`seal` produced; raises :class:`SealError` on a wrong secret, label or any tampering."""
    raw = _unb64url(sealed)
    if len(raw) < NONCE_LEN + TAG_LEN:
        raise SealError("too short")
    ke, km = keys(secret, label)
    body, tag = raw[:-TAG_LEN], raw[-TAG_LEN:]
    if not hmac.compare_digest(_hmac(km, body), tag):
        raise SealError("bad tag: wrong secret or tampered")
    return xor_keystream(ke, body[:NONCE_LEN], body[NONCE_LEN:])

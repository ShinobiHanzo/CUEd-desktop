"""
``cued://share``, ``cued://station`` and ``cued://pair`` links.

A faithful port of the phone's codecs (``core/.../share/SharePayload.kt`` and
``core/.../station/Station.kt``) plus the pairing link from
``docs/protocol.md`` §2. The percent-encoding is Java's ``URLEncoder`` with
``+`` turned into ``%20``: alphanumerics and ``- _ . *`` pass through,
everything else (including ``~`` and ``/``) is ``%XX`` encoded, uppercase hex.
Decoding accepts ``+`` as a space like Java's ``URLDecoder``.

Every ``decode`` returns ``None`` when the text is not that kind of link, as
the Kotlin does, so callers can try the three in turn (or use
:func:`decode_link`).
"""
from __future__ import annotations

import hashlib
import hmac
import os
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple, Union
from urllib.parse import unquote_plus

SCHEME = "cued"
WEB_BASE = "https://shinobihanzo.github.io/CUEd/"
SHARE_VERSION = 1
PAIR_VERSION = 1

_SAFE = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.*")


def enc(s: str) -> str:
    """``URLEncoder.encode(s, "UTF-8").replace("+", "%20")``."""
    out = []
    for b in s.encode("utf-8"):
        if b in _SAFE:
            out.append(chr(b))
        else:
            out.append("%%%02X" % b)
    return "".join(out)


def dec(s: str) -> str:
    """``URLDecoder.decode(s, "UTF-8")``: ``+`` is a space, ``%XX`` is a byte."""
    return unquote_plus(s, encoding="utf-8", errors="replace")


def parse_query(q: str) -> Dict[str, str]:
    """``k=v&k=v`` into a dict; a pair without ``=`` (or an empty key) is skipped. Last wins."""
    out: Dict[str, str] = {}
    for pair in q.split("&"):
        i = pair.find("=")
        if i <= 0:
            continue
        out[pair[:i]] = dec(pair[i + 1 :])
    return out


def build_query(pairs: Sequence[Tuple[str, str]]) -> str:
    return "&".join("%s=%s" % (k, enc(v)) for k, v in pairs)


def is_hex(s: str, length: int) -> bool:
    if len(s) != length:
        return False
    try:
        bytes.fromhex(s)
    except ValueError:
        return False
    return True


def _fmt_bpm(bpm: float) -> str:
    """Kotlin ``"%.1f".format(Locale.ROOT, bpm)``; the phone stores bpm as a Float."""
    return "%.1f" % bpm


# ---------------------------------------------------------------- share


@dataclass
class SharePayload:
    """One track handed over QR / NFC / paste. See CUEd ``docs/sharing.md``."""

    title: str
    artist: str = ""
    link: Optional[str] = None
    file_url: Optional[str] = None
    apk_url: Optional[str] = None
    genres: List[str] = field(default_factory=list)
    bpm: Optional[float] = None
    version: int = SHARE_VERSION

    PREFIX = SCHEME + "://share?"

    def _query(self) -> str:
        q: List[Tuple[str, str]] = [("v", str(self.version)), ("t", self.title), ("a", self.artist)]
        if self.link is not None:
            q.append(("l", self.link))
        if self.file_url is not None:
            q.append(("f", self.file_url))
        if self.apk_url is not None:
            q.append(("k", self.apk_url))
        if self.genres:
            q.append(("g", ",".join(self.genres)))
        if self.bpm is not None:
            q.append(("b", _fmt_bpm(self.bpm)))
        return build_query(q)

    def encode(self) -> str:
        return self.PREFIX + self._query()

    def encode_web(self) -> str:
        """The same fields behind the download page: ``https://…/CUEd/#v=1&t=…``."""
        return WEB_BASE + "#" + self._query()

    @classmethod
    def decode(cls, text: str) -> Optional["SharePayload"]:
        """Accepts ``cued://share?…`` and the web form ``…/CUEd/#…`` (or ``?…``)."""
        t = text.strip()
        if t.startswith(cls.PREFIX):
            query = t[len(cls.PREFIX) :]
        elif t[: len(WEB_BASE)].lower() == WEB_BASE.lower():
            query = t[len(WEB_BASE) :].lstrip("#?")
        else:
            return None
        q = parse_query(query)
        title = q.get("t")
        if title is None:
            return None
        bpm: Optional[float] = None
        if "b" in q:
            try:
                bpm = float(q["b"])
            except ValueError:
                bpm = None
        try:
            version = int(q["v"]) if "v" in q else SHARE_VERSION
        except ValueError:
            version = SHARE_VERSION
        return cls(
            title=title,
            artist=q.get("a", ""),
            link=q.get("l"),
            file_url=q.get("f"),
            apk_url=q.get("k"),
            genres=[g for g in q["g"].split(",") if g.strip()] if "g" in q else [],
            bpm=bpm,
            version=version,
        )


# ---------------------------------------------------------------- station


@dataclass
class StationLink:
    """``cued://station?p=<pubkey hex>&n=<name>&r=<relay,relay>``."""

    pubkey: str
    name: str = ""
    relays: List[str] = field(default_factory=list)

    PREFIX = SCHEME + "://station?"

    def encode(self) -> str:
        q: List[Tuple[str, str]] = [("p", self.pubkey)]
        if self.name.strip():
            q.append(("n", self.name))
        if self.relays:
            q.append(("r", ",".join(self.relays)))
        return self.PREFIX + build_query(q)

    @classmethod
    def decode(cls, text: str) -> Optional["StationLink"]:
        t = text.strip()
        if not t.startswith(cls.PREFIX):
            return None
        q = parse_query(t[len(cls.PREFIX) :])
        p = q.get("p")
        if p is None:
            return None
        p = p.lower()
        if not is_hex(p, 64):
            return None
        relays = [r for r in q["r"].split(",") if r.strip()] if "r" in q else []
        return cls(p, q.get("n", ""), relays)


# ---------------------------------------------------------------- pair


@dataclass
class PairLink:
    """``cued://pair?v=1&n=&a=&f=&c=&s=&k=&r=`` (protocol.md §2)."""

    secret: str
    name: str = ""
    addresses: List[str] = field(default_factory=list)
    funnel: Optional[str] = None
    cert_sha256: Optional[str] = None
    pubkey: Optional[str] = None
    relay: Optional[str] = None
    version: int = PAIR_VERSION

    PREFIX = SCHEME + "://pair?"

    def encode(self) -> str:
        q: List[Tuple[str, str]] = [("v", str(self.version)), ("n", self.name), ("a", ",".join(self.addresses))]
        if self.funnel is not None:
            q.append(("f", self.funnel))
        if self.cert_sha256 is not None:
            q.append(("c", self.cert_sha256))
        q.append(("s", self.secret))
        if self.pubkey is not None:
            q.append(("k", self.pubkey))
        if self.relay is not None:
            q.append(("r", self.relay))
        return self.PREFIX + build_query(q)

    @classmethod
    def decode(cls, text: str) -> Optional["PairLink"]:
        t = text.strip()
        if not t.startswith(cls.PREFIX):
            return None
        q = parse_query(t[len(cls.PREFIX) :])
        secret = q.get("s", "").lower()
        if not is_hex(secret, 32):
            return None
        pubkey = q.get("k")
        if pubkey is not None:
            pubkey = pubkey.lower()
            if not is_hex(pubkey, 64):
                return None
        try:
            version = int(q["v"]) if "v" in q else PAIR_VERSION
        except ValueError:
            version = PAIR_VERSION
        cert = q.get("c")
        return cls(
            secret=secret,
            name=q.get("n", ""),
            addresses=[a for a in q["a"].split(",") if a] if "a" in q else [],
            funnel=q.get("f"),
            cert_sha256=cert.lower() if cert is not None else None,
            pubkey=pubkey,
            relay=q.get("r"),
            version=version,
        )


def pair_proof(secret_hex: str, device: str, name: str, nonce: str) -> str:
    """``HMAC-SHA256(key = bytes(s), msg = device + '|' + name + '|' + nonce)`` as hex."""
    key = bytes.fromhex(secret_hex)
    msg = ("%s|%s|%s" % (device, name, nonce)).encode("utf-8")
    return hmac.new(key, msg, hashlib.sha256).hexdigest()


def verify_pair_proof(secret_hex: str, device: str, name: str, nonce: str, proof: str) -> bool:
    try:
        expected = pair_proof(secret_hex, device, name, nonce)
    except ValueError:
        return False
    return hmac.compare_digest(expected, proof.lower())


def build_pair_request(link: PairLink, name: str, device: str, pubkey: str = "", nonce: Optional[str] = None) -> dict:
    """The JSON body a phone POSTs to ``/api/pair`` after scanning ``link``."""
    if nonce is None:
        nonce = os.urandom(8).hex()
    return {
        "v": PAIR_VERSION,
        "name": name,
        "device": device,
        "pubkey": pubkey,
        "nonce": nonce,
        "proof": pair_proof(link.secret, device, name, nonce),
    }


def new_pair_secret() -> str:
    return os.urandom(16).hex()


# ---------------------------------------------------------------- any link

Link = Union[SharePayload, StationLink, PairLink]


def decode_link(text: str) -> Optional[Link]:
    """Recognises every CUEd link form, including the web share form."""
    return PairLink.decode(text) or StationLink.decode(text) or SharePayload.decode(text)

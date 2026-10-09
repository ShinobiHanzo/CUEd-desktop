"""
The account chain (``docs/protocol.md`` §8): a signed, hash-linked list of
blocks only the account key can extend. Every device of the account holds a
full copy; copies merge by "longer valid chain wins". Mirrors
``crates/cued-proto/src/account.rs``.

Canonical form hashed into a block: ``[v, height, prev, ts, type, data, pubkey]``
serialised with no whitespace and object keys in insertion order (what
``serde_json`` and ``kotlinx.serialization`` both produce), so keep ``data``
dicts in the order they were built and never sort keys.
"""
from __future__ import annotations

import copy
import hashlib
import json
import os
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

from . import nostr

BLOCK_VERSION = 1

TYPE_CREATE = "create"
TYPE_PROFILE = "profile"
TYPE_BIND = "bind_device"
TYPE_UNBIND = "unbind_device"
TYPE_SETTINGS = "settings"


class ChainError(ValueError):
    """A block or chain that does not validate, or a key that may not extend it."""


def _dumps(obj: Any) -> str:
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False)


def canonical(v: int, height: int, prev: str, ts: int, type_: str, data: Dict[str, Any], pubkey: str) -> str:
    """The exact string whose SHA-256 is the block hash."""
    return _dumps([v, height, prev, ts, type_, data, pubkey])


def block_hash(v: int, height: int, prev: str, ts: int, type_: str, data: Dict[str, Any], pubkey: str) -> str:
    return hashlib.sha256(canonical(v, height, prev, ts, type_, data, pubkey).encode("utf-8")).hexdigest()


def devhash(device_id: str, salt: str) -> str:
    """``sha256("cued-device|" + device id + "|" + salt)`` hex: a unique, opaque handle for a device."""
    return hashlib.sha256(("cued-device|%s|%s" % (device_id, salt)).encode("utf-8")).hexdigest()


def account_id(pubkey_hex: str) -> str:
    """``sha256(pubkey bytes)`` hex."""
    try:
        raw = bytes.fromhex(pubkey_hex)
    except ValueError:
        raw = b""
    return hashlib.sha256(raw).hexdigest()


def new_salt() -> str:
    return os.urandom(16).hex()


@dataclass
class Block:
    v: int
    height: int
    prev: str
    ts: int
    type: str
    data: Dict[str, Any]
    pubkey: str
    hash: str
    sig: str

    def to_dict(self) -> Dict[str, Any]:
        return {"v": self.v, "height": self.height, "prev": self.prev, "ts": self.ts, "type": self.type,
                "data": copy.deepcopy(self.data), "pubkey": self.pubkey, "hash": self.hash, "sig": self.sig}

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> "Block":
        try:
            data = d["data"]
            if not isinstance(data, dict):
                raise ChainError("block data must be an object")
            return cls(v=int(d["v"]), height=int(d["height"]), prev=str(d["prev"]), ts=int(d["ts"]), type=str(d["type"]),
                       data=copy.deepcopy(data), pubkey=str(d["pubkey"]), hash=str(d["hash"]), sig=str(d["sig"]))
        except (KeyError, TypeError, ValueError) as e:
            raise ChainError("malformed block: %s" % e)

    def compute_hash(self) -> str:
        return block_hash(self.v, self.height, self.prev, self.ts, self.type, self.data, self.pubkey)

    def verify(self) -> bool:
        """Hash matches the fields and the signature is the account key's (BIP-340 over the 32-byte hash)."""
        if self.v != BLOCK_VERSION or self.hash != self.compute_hash():
            return False
        try:
            return nostr.verify(bytes.fromhex(self.hash), bytes.fromhex(self.pubkey), bytes.fromhex(self.sig))
        except ValueError:
            return False


def sign_block(secret_hex: str, height: int, prev: str, ts: int, type_: str, data: Dict[str, Any], aux: Optional[bytes] = None) -> Block:
    """Builds and signs one block with the account secret; ``data`` keeps its insertion order."""
    sk = bytes.fromhex(secret_hex)
    pubkey = nostr.public_key(sk).hex()
    h = block_hash(BLOCK_VERSION, height, prev, ts, type_, data, pubkey)
    sig = nostr.sign(bytes.fromhex(h), sk, aux).hex()
    return Block(BLOCK_VERSION, height, prev, ts, type_, data, pubkey, h, sig)


@dataclass
class Chain:
    blocks: List[Block] = field(default_factory=list)

    # -- construction

    @classmethod
    def create(cls, secret_hex: str, name: str, ts: int) -> "Chain":
        """A new account: block 0 is ``create {name}``."""
        c = cls()
        c.append(secret_hex, TYPE_CREATE, {"name": name}, ts)
        return c

    def append(self, secret_hex: str, type_: str, data: Dict[str, Any], ts: int) -> Block:
        """Signs and appends a block. ``secret_hex`` must be the account's key."""
        pubkey = nostr.public_key(bytes.fromhex(secret_hex)).hex()
        if self.pubkey is not None and self.pubkey != pubkey:
            raise ChainError("only the account key can extend its chain")
        b = sign_block(secret_hex, self.height, self.tip_hash, ts, type_, data)
        self.blocks.append(b)
        return b

    # -- shape

    @property
    def pubkey(self) -> Optional[str]:
        return self.blocks[0].pubkey if self.blocks else None

    @property
    def height(self) -> int:
        return len(self.blocks)

    @property
    def tip(self) -> Optional[Block]:
        return self.blocks[-1] if self.blocks else None

    @property
    def tip_hash(self) -> str:
        return self.blocks[-1].hash if self.blocks else ""

    def validate(self) -> None:
        """Every block well-formed, linked, consecutive and signed by block 0's key; raises :class:`ChainError`."""
        if not self.blocks:
            raise ChainError("empty")
        first = self.blocks[0]
        if first.type != TYPE_CREATE or first.prev != "" or first.height != 0:
            raise ChainError("block 0 must be create")
        prev = ""
        for i, b in enumerate(self.blocks):
            if b.height != i or b.prev != prev or b.pubkey != first.pubkey or not b.verify():
                raise ChainError("block %d is not valid" % i)
            prev = b.hash

    def is_valid(self) -> bool:
        try:
            self.validate()
        except ChainError:
            return False
        return True

    def merge(self, other: "Chain") -> bool:
        """Longer valid chain wins (equal height: lower tip hash). Returns True when self changed. Same account only."""
        other.validate()
        if not self.blocks:
            self.blocks = copy.deepcopy(other.blocks)
            return True
        if self.pubkey != other.pubkey:
            raise ChainError("different account")
        take_other = other.height > self.height or (other.height == self.height and other.tip_hash < self.tip_hash)
        if take_other and other.tip_hash != self.tip_hash:
            self.blocks = copy.deepcopy(other.blocks)
            return True
        return False

    # -- derived views

    def latest(self, type_: str) -> Optional[Dict[str, Any]]:
        """``data`` of the newest block of this type."""
        for b in reversed(self.blocks):
            if b.type == type_:
                return b.data
        return None

    def name(self) -> str:
        d = self.latest(TYPE_PROFILE) or self.latest(TYPE_CREATE) or {}
        n = d.get("name")
        return n if isinstance(n, str) else ""

    def devices(self) -> List[Dict[str, Any]]:
        """Devices currently bound: ``bind_device`` minus later ``unbind_device``; each with ``boundAt``."""
        out: List[Dict[str, Any]] = []
        for b in self.blocks:
            if b.type == TYPE_BIND:
                dh = b.data.get("devhash", "")
                out = [d for d in out if d.get("devhash") != dh]
                d = dict(b.data)
                d["boundAt"] = b.ts
                out.append(d)
            elif b.type == TYPE_UNBIND:
                dh = b.data.get("devhash", "")
                out = [d for d in out if d.get("devhash") != dh]
        return out

    def settings(self) -> Dict[str, Any]:
        """Every ``settings`` block merged in order (later keys win)."""
        acc: Dict[str, Any] = {}
        for b in self.blocks:
            if b.type == TYPE_SETTINGS:
                acc.update(copy.deepcopy(b.data))
        return acc

    # -- JSON

    def to_dict(self) -> Dict[str, Any]:
        return {"blocks": [b.to_dict() for b in self.blocks]}

    def to_json(self) -> str:
        return _dumps(self.to_dict())

    @classmethod
    def from_dict(cls, d: Any) -> "Chain":
        """Accepts ``{"blocks":[…]}`` or a bare list of blocks."""
        blocks = d.get("blocks") if isinstance(d, dict) else d
        if not isinstance(blocks, list):
            raise ChainError("chain must be {\"blocks\": [...]}")
        return cls([Block.from_dict(b) for b in blocks])

    @classmethod
    def from_json(cls, text: str) -> "Chain":
        try:
            return cls.from_dict(json.loads(text))
        except ValueError as e:
            raise ChainError("bad chain json: %s" % e)

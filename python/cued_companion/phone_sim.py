#!/usr/bin/env python3
"""
A command-line stand-in for the phone, so the desktop (``cuedd``) can be
driven end to end without Android. Speaks exactly what ``docs/protocol.md``
says the phone speaks: ``cued://pair`` with the HMAC proof, the sync manifest,
resumable chunked uploads, backup blobs and snapshots, and signed kind-30777
station states over the relay websocket.

    cued-phone-sim pair "cued://pair?…" [--name "Sim Phone"] [--account join|keep|separate]
    cued-phone-sim sync <dir>
    cued-phone-sim receive "cued://share?…" [--dir received]
    cued-phone-sim station --relay ws://host:8770/relay --name X --title T --artist A [--seconds 30]
    cued-phone-sim blob put <name> <file> | blob get <name> <file>
    cued-phone-sim snapshot
    cued-phone-sim status
    cued-phone-sim account show | chain | push
    cued-phone-sim settings get | set '<json>'
    cued-phone-sim friends list | requests | link | add <link-or-npub> | accept <pubkey> | remove <pubkey>
    cued-phone-sim listen <pubkey-or-npub> | stop | status
    cued-phone-sim lyrics <sha256>
    cued-phone-sim cover <sha256> <out file>

State (token, device id, desktop URL, identity, the "biometric" P-256 key, the
device salt, the account chain and the cached assertion) lives in ``--state``
(default ``~/.cued-phone-sim.json``). Only the standard library is used;
``station`` needs the optional ``websockets`` package.

When the paired base is ``https://`` (the funnel) the sim is "away from home"
(protocol.md §7): every request carries ``X-Cued-Assertion`` signed with the
biometric key against a fresh ``/api/auth/challenge`` (cached nine minutes),
and anything that writes is refused here before the desktop gets to say 403.

Exit codes: 0 ok, 1 protocol / usage failure, 2 network failure, 3 missing
dependency. Every failure prints one ``error: …`` line on stderr.
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import http.client
import json
import os
import re
import shutil
import socket
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.request
import wave
from typing import Callable, Dict, List, Optional, Tuple
from urllib.parse import quote, urlsplit

from . import account as account_mod
from . import bio, links, nostr, seal

DEFAULT_STATE = os.path.join("~", ".cued-phone-sim.json")
AUDIO_EXT = {".mp3", ".m4a", ".flac", ".ogg", ".opus", ".wav"}
MIME = {".mp3": "audio/mpeg", ".m4a": "audio/mp4", ".flac": "audio/flac", ".ogg": "audio/ogg", ".opus": "audio/ogg", ".wav": "audio/wav"}
CHUNK_SIZE = 1 << 20
CONNECT_TIMEOUT = 3.0
REQUEST_TIMEOUT = 30.0
UPLOAD_TIMEOUT = 120.0
BLOB_NAME = re.compile(r"^[A-Za-z0-9._-]{1,80}$")
ASSERTION_TTL = 9 * 60          # the desktop accepts one for 10 minutes; refresh a minute early
ASSERTION_HEADER = "X-Cued-Assertion"


class SimError(Exception):
    """Something the protocol or the user got wrong (exit 1)."""

    code = 1


class NetError(SimError):
    """The desktop could not be reached or answered garbage (exit 2)."""

    code = 2


class MissingDependency(SimError):
    code = 3


# ---------------------------------------------------------------- state


def load_state(path: str) -> dict:
    p = os.path.expanduser(path)
    if not os.path.exists(p):
        return {}
    try:
        with open(p, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError) as e:
        raise SimError("cannot read state file %s: %s" % (p, e))
    return data if isinstance(data, dict) else {}


def save_state(path: str, state: dict) -> None:
    p = os.path.expanduser(path)
    tmp = p + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=2)
        f.write("\n")
    try:
        os.chmod(tmp, 0o600)
    except OSError:
        pass
    os.replace(tmp, p)


def ensure_device(state: dict) -> str:
    if not state.get("device"):
        state["device"] = os.urandom(8).hex()
    return state["device"]


def ensure_identity(state: dict) -> Tuple[str, str]:
    """(secret hex, pubkey hex); generated on first use like the phone's station key."""
    ident = state.get("identity") or {}
    if not ident.get("secret"):
        sk = nostr.generate_secret()
        ident = {"secret": sk.hex(), "pubkey": nostr.public_key(sk).hex()}
        state["identity"] = ident
    return ident["secret"], ident["pubkey"]


def ensure_biokey(state: dict) -> Tuple[str, str]:
    """(private hex, 65-byte SEC1 public hex): the sim's stand-in for the Android Keystore key (protocol.md §7)."""
    bk = state.get("biokey") or {}
    if not bk.get("priv") or not bk.get("pub"):
        priv, pub = bio.generate()
        bk = {"priv": priv, "pub": pub}
        state["biokey"] = bk
    return bk["priv"], bk["pub"]


def ensure_devhash(state: dict) -> str:
    """``sha256("cued-device|" + device id + "|" + salt)`` with a per-install salt kept in the state file."""
    device = ensure_device(state)
    if not state.get("devsalt"):
        state["devsalt"] = account_mod.new_salt()
    return account_mod.devhash(device, state["devsalt"])


def load_chain(state: dict) -> Optional[account_mod.Chain]:
    raw = state.get("chain")
    if not raw:
        return None
    try:
        return account_mod.Chain.from_dict(raw)
    except account_mod.ChainError as e:
        raise SimError("the chain in the state file is unreadable: %s" % e)


def store_chain(state: dict, chain: account_mod.Chain) -> None:
    state["chain"] = chain.to_dict()


def ensure_chain(state: dict, name: str) -> account_mod.Chain:
    """The sim's account chain; created on first use as ``create {name}`` + ``bind_device`` for this phone."""
    chain = load_chain(state)
    if chain is None:
        secret, _ = ensure_identity(state)
        ts = int(time.time())
        chain = account_mod.Chain.create(secret, name, ts)
        chain.append(secret, account_mod.TYPE_BIND, {"devhash": ensure_devhash(state), "name": name, "kind": "phone"}, ts)
        store_chain(state, chain)
    return chain


def require_paired(state: dict) -> Tuple[str, str]:
    base, token = state.get("desktop"), state.get("token")
    if not base or not token:
        raise SimError("not paired yet: run  pair \"cued://pair?…\"  first")
    return base, token


def is_remote(base: Optional[str]) -> bool:
    """An ``https://`` base is the funnel: the desktop is read-only from there (protocol.md §7)."""
    return bool(base) and base.lower().startswith("https://")


def require_lan(base: str, what: str) -> None:
    if is_remote(base):
        raise SimError("%s only on the LAN: the desktop is read-only through the funnel (%s); pair over Wi-Fi for writes" % (what, base))


# ---------------------------------------------------------------- HTTP (urllib only)


class _PinnedHTTPSConnection(http.client.HTTPSConnection):
    """TLS without a CA: the only thing we trust is the SHA-256 of the DER cert the QR carried."""

    pin: str = ""

    def connect(self) -> None:
        super().connect()
        der = self.sock.getpeercert(binary_form=True)  # type: ignore[union-attr]
        got = hashlib.sha256(der or b"").hexdigest()
        if got != self.pin.lower():
            self.close()
            raise ssl.SSLError("certificate pin mismatch: desktop presented %s, QR said %s" % (got, self.pin))


class _PinnedHTTPSHandler(urllib.request.HTTPSHandler):
    def __init__(self, pin: str):
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        super().__init__(context=ctx)
        self._pin = pin

    def https_open(self, req):
        pin = self._pin

        def factory(host, **kw):
            c = _PinnedHTTPSConnection(host, **kw)
            c.pin = pin
            return c

        return self.do_open(factory, req, context=self._context)


# Fingerprint of the desktop's funnel certificate once paired through it; every later request pins it.
DEFAULT_PIN: Optional[str] = None
# Headers every authenticated request carries besides the bearer: the biometric assertion when remote.
EXTRA_HEADERS: Dict[str, str] = {}


def pinned_ssl_context() -> ssl.SSLContext:
    """TLS with no CA check at all; the caller compares the peer's DER SHA-256 to the pin itself."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def make_opener(cert_sha256: Optional[str] = None) -> urllib.request.OpenerDirector:
    """No proxies (this is LAN traffic), optional cert pinning for the funnel."""
    handlers: list = [urllib.request.ProxyHandler({})]
    if cert_sha256 is None:
        cert_sha256 = DEFAULT_PIN
    if cert_sha256:
        handlers.append(_PinnedHTTPSHandler(cert_sha256))
    return urllib.request.build_opener(*handlers)


Response = Tuple[int, Dict[str, str], bytes]


def request(method: str, url: str, body: Optional[bytes] = None, headers: Optional[Dict[str, str]] = None,
            timeout: float = REQUEST_TIMEOUT, opener: Optional[urllib.request.OpenerDirector] = None) -> Response:
    """(status, lower-cased headers, body). HTTP errors come back as statuses; transport errors raise NetError."""
    req = urllib.request.Request(url, data=body, method=method)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    if body is not None and "Content-Type" not in (headers or {}):
        req.add_header("Content-Type", "application/octet-stream")
    op = opener or make_opener()
    try:
        with op.open(req, timeout=timeout) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as e:
        data = e.read() if e.fp is not None else b""
        return e.code, {k.lower(): v for k, v in e.headers.items()}, data
    except (urllib.error.URLError, socket.timeout, TimeoutError, ConnectionError, ssl.SSLError, http.client.HTTPException, OSError) as e:
        reason = getattr(e, "reason", None) or e
        raise NetError("%s %s: %s" % (method, url, reason))


def request_json(method: str, url: str, obj=None, token: Optional[str] = None, timeout: float = REQUEST_TIMEOUT,
                 opener=None, extra_headers: Optional[Dict[str, str]] = None) -> Tuple[int, object]:
    headers = dict(extra_headers or {})
    body = None
    if obj is not None:
        body = json.dumps(obj).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if token:
        headers.update(_bearer(token))
    status, _, data = request(method, url, body, headers, timeout, opener)
    parsed: object = None
    if data:
        try:
            parsed = json.loads(data.decode("utf-8"))
        except ValueError:
            parsed = data.decode("utf-8", "replace")
    return status, parsed


def _bearer(token: str) -> Dict[str, str]:
    """The bearer plus whatever else the current tier needs (the assertion, away from home)."""
    h = {"Authorization": "Bearer " + token}
    h.update(EXTRA_HEADERS)
    return h


def fetch_assertion(base: str, token: str, state: dict) -> str:
    """``GET /api/auth/challenge`` (bearer only), sign ``cued-bio|device|challenge`` with the biometric key."""
    priv, _ = ensure_biokey(state)
    device = ensure_device(state)
    saved = dict(EXTRA_HEADERS)
    EXTRA_HEADERS.clear()  # the challenge itself needs no assertion
    try:
        status, resp = request_json("GET", base + "/api/auth/challenge", None, token)
    finally:
        EXTRA_HEADERS.update(saved)
    if status != 200 or not isinstance(resp, dict) or not isinstance(resp.get("challenge"), str):
        raise SimError("challenge: " + _describe(status, resp))
    return bio.assertion(priv, device, resp["challenge"])


def ensure_assertion(state: dict, force: bool = False) -> str:
    """A cached assertion, refreshed when older than nine minutes; installs it into EXTRA_HEADERS."""
    base, token = require_paired(state)
    cached = state.get("assertion") or {}
    now = int(time.time())
    value = cached.get("value")
    if force or not value or int(cached.get("expires") or 0) <= now:
        value = fetch_assertion(base, token, state)
        state["assertion"] = {"value": value, "expires": now + ASSERTION_TTL}
    EXTRA_HEADERS[ASSERTION_HEADER] = value
    return value


def _describe(status: int, body) -> str:
    if isinstance(body, dict) and body.get("error"):
        return "HTTP %d: %s" % (status, body["error"])
    if isinstance(body, str) and body.strip():
        return "HTTP %d: %s" % (status, body.strip()[:200])
    return "HTTP %d" % status


# ---------------------------------------------------------------- pair


def pair_with(link: links.PairLink, name: str, device: str, pubkey: str, account: Optional[str] = None,
              biokey: Optional[str] = None, devhash: Optional[str] = None) -> Tuple[str, dict, Optional[str]]:
    """Try each LAN address, then the funnel with its cert pinned. Returns (base url used, response json, pin used)."""
    body = links.build_pair_request(link, name, device, pubkey, account=account, biokey=biokey, devhash=devhash)
    attempts: List[Tuple[str, Optional[str]]] = [(a, None) for a in link.addresses]
    if link.funnel:
        attempts.append((link.funnel, link.cert_sha256))
    if not attempts:
        raise SimError("pair link has no addresses")
    errors: List[str] = []
    for base, pin in attempts:
        base = base.rstrip("/")
        try:
            status, resp = request_json("POST", base + "/api/pair", body, timeout=CONNECT_TIMEOUT, opener=make_opener(pin))
        except NetError as e:
            errors.append(str(e))
            continue
        if status == 200 and isinstance(resp, dict) and resp.get("token"):
            return base, resp, pin
        if status == 403:
            reason = resp.get("error", "") if isinstance(resp, dict) else ""
            if "LAN" in reason:
                raise SimError("desktop refused the pairing from %s: %s (pair on the desktop's own Wi-Fi)" % (base, reason))
            raise SimError("desktop refused the pairing (wrong or expired secret); show a fresh QR")
        if status == 409:
            raise SimError("this device id (%s) is already paired; unlink it on the desktop first" % device)
        errors.append("%s -> %s" % (base, _describe(status, resp)))
    raise NetError("no address answered: " + "; ".join(errors))


def account_bundle(secret_hex: str, chain: account_mod.Chain) -> dict:
    """What travels inside the ``account`` seal, both ways: the key and the whole chain."""
    return {"secret_hex": secret_hex, "chain": chain.to_dict()}


def open_account_bundle(pair_secret_hex: str, sealed: str) -> Tuple[str, account_mod.Chain]:
    """Opens ``account.sealed`` from the pair response with the pairing secret; returns (secret hex, chain)."""
    try:
        raw = seal.open_sealed(bytes.fromhex(pair_secret_hex), seal.LABEL_ACCOUNT, sealed)
        bundle = json.loads(raw.decode("utf-8"))
        secret_hex = bundle["secret_hex"]
        chain = account_mod.Chain.from_dict(bundle["chain"])
        chain.validate()
    except seal.SealError as e:
        raise SimError("account bundle: %s" % e)
    except (ValueError, KeyError, TypeError, account_mod.ChainError) as e:
        raise SimError("account bundle is not a {secret_hex, chain} the sim understands: %s" % e)
    if not nostr.is_valid_secret(bytes.fromhex(secret_hex)) or nostr.public_key(bytes.fromhex(secret_hex)).hex() != chain.pubkey:
        raise SimError("account bundle: the key does not match the chain")
    return secret_hex, chain


def adopt_account(state: dict, secret_hex: str, chain: account_mod.Chain) -> None:
    """``join``: the sim becomes the desktop's account."""
    state["identity"] = {"secret": secret_hex, "pubkey": chain.pubkey}
    store_chain(state, chain)


def import_account(base: str, token: str, pair_secret_hex: str, secret_hex: str, chain: account_mod.Chain) -> dict:
    """``keep``: ``POST /api/account/import`` with the sim's key and chain sealed under the pairing secret (LAN only)."""
    require_lan(base, "account import")
    sealed = seal.seal(bytes.fromhex(pair_secret_hex), seal.LABEL_ACCOUNT, json.dumps(account_bundle(secret_hex, chain), separators=(",", ":")).encode("utf-8"))
    status, resp = request_json("POST", base + "/api/account/import", {"sealed": sealed}, token)
    if status not in (200, 201):
        raise SimError("account import: " + _describe(status, resp))
    return resp if isinstance(resp, dict) else {}


def cmd_pair(args, state: dict) -> int:
    link = links.PairLink.decode(args.link)
    if link is None:
        raise SimError("not a cued://pair link")
    device = ensure_device(state)
    _, pubkey = ensure_identity(state)
    _, biokey_pub = ensure_biokey(state)
    devhash = ensure_devhash(state)
    mode = args.account
    base, resp, pin = pair_with(link, args.name, device, pubkey, account=mode, biokey=biokey_pub, devhash=devhash)
    state.update({"desktop": base, "token": resp["token"], "name": args.name, "desktop_info": resp.get("desktop", {}), "paired_at": int(time.time()),
                  "cert": pin, "account_mode": mode})
    state.pop("assertion", None)
    global DEFAULT_PIN
    DEFAULT_PIN = pin
    save_state(args.state, state)  # the token is ours now, whatever happens to the account step
    info = resp.get("desktop", {})
    print("paired with %s at %s (relay %s, device %s)" % (info.get("name") or link.name or "desktop", base, info.get("relay") or link.relay or "-", device))
    print("biokey %s… registered, devhash %s…" % (biokey_pub[:16], devhash[:16]))
    if mode == "join":
        sealed = (resp.get("account") or {}).get("sealed") if isinstance(resp.get("account"), dict) else None
        if not sealed:
            raise SimError("asked to join the desktop's account but the pair response carried no account.sealed")
        secret_hex, chain = open_account_bundle(link.secret, sealed)
        adopt_account(state, secret_hex, chain)
        save_state(args.state, state)
        print("joined account %s (%s): %d block(s), %d device(s), name %r" % (account_mod.account_id(chain.pubkey)[:16], nostr.npub(chain.pubkey), chain.height, len(chain.devices()), chain.name()))
    elif mode == "keep":
        chain = ensure_chain(state, args.name)
        secret_hex, _ = ensure_identity(state)
        save_state(args.state, state)
        result = import_account(base, resp["token"], link.secret, secret_hex, chain)
        print("desktop now uses this phone's account %s (%s); it answered %s" % (account_mod.account_id(chain.pubkey)[:16], nostr.npub(chain.pubkey), json.dumps(result) if result else "ok"))
    else:
        print("separate accounts: paired devices, nothing shared")
    return 0


# ---------------------------------------------------------------- scanning a music folder


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def ffprobe_path() -> Optional[str]:
    return shutil.which("ffprobe")


def probe_ffprobe(path: str, exe: str) -> dict:
    """{title, artist, album, durationMs} from ffprobe, best effort (empty dict on any trouble)."""
    try:
        r = subprocess.run([exe, "-v", "quiet", "-print_format", "json", "-show_format", path], capture_output=True, timeout=20)
        info = json.loads(r.stdout.decode("utf-8", "replace") or "{}").get("format", {})
    except (OSError, subprocess.SubprocessError, ValueError):
        return {}
    tags = {k.lower(): v for k, v in (info.get("tags") or {}).items()}
    out: dict = {}
    try:
        out["durationMs"] = int(float(info.get("duration", 0)) * 1000)
    except (TypeError, ValueError):
        pass
    for k in ("title", "artist", "album"):
        if tags.get(k):
            out[k] = str(tags[k])
    return out


def probe_wav(path: str) -> dict:
    try:
        with wave.open(path, "rb") as w:
            rate = w.getframerate()
            return {"durationMs": int(w.getnframes() * 1000 / rate)} if rate else {}
    except (wave.Error, EOFError, OSError):
        return {}


def guess_from_filename(path: str) -> Tuple[str, str]:
    """``Artist - Title.ext`` -> (artist, title); otherwise ("", stem)."""
    stem = os.path.splitext(os.path.basename(path))[0]
    if " - " in stem:
        artist, title = stem.split(" - ", 1)
        return artist.strip(), title.strip()
    return "", stem.strip()


def manifest_entry(path: str, root: str, probe: Optional[Callable[[str], dict]] = None) -> dict:
    ext = os.path.splitext(path)[1].lower()
    st = os.stat(path)
    info: dict = {}
    if probe is not None:
        info = probe(path) or {}
    elif ext == ".wav":
        info = probe_wav(path)
    artist, title = guess_from_filename(path)
    return {
        "key": os.path.relpath(path, root).replace(os.sep, "/"),
        "title": info.get("title") or title,
        "artist": info.get("artist") or artist,
        "album": info.get("album") or "",
        "durationMs": int(info.get("durationMs") or 0),
        "size": st.st_size,
        "sha256": sha256_file(path),
        "mime": MIME.get(ext, "application/octet-stream"),
        "genres": [],
        "bpm": None,
        "link": None,
        "modified": int(st.st_mtime * 1000),
    }


def scan_dir(root: str, use_ffprobe: Optional[bool] = None) -> List[dict]:
    """Every audio file under ``root`` as a manifest entry. ``use_ffprobe=None`` means "if installed"."""
    root = os.path.abspath(root)
    if not os.path.isdir(root):
        raise SimError("not a directory: %s" % root)
    exe = ffprobe_path() if use_ffprobe is not False else None
    if use_ffprobe and not exe:
        raise SimError("ffprobe requested but not on PATH")
    probe = (lambda p: probe_ffprobe(p, exe)) if exe else None
    out: List[dict] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for name in sorted(filenames):
            if os.path.splitext(name)[1].lower() in AUDIO_EXT:
                out.append(manifest_entry(os.path.join(dirpath, name), root, probe))
    return out


# ---------------------------------------------------------------- sync


def upload_track(base: str, token: str, entry: dict, path: str, chunk_size: int = CHUNK_SIZE,
                 log: Callable[[str], None] = lambda s: None) -> str:
    """HEAD for what the desktop has, then PUT the rest in Content-Range chunks. Returns 'already' or 'uploaded'."""
    sha = entry["sha256"]
    total = entry["size"]
    url = "%s/api/sync/track/%s" % (base, sha)
    status, headers, body = request("HEAD", url, headers=_bearer(token))
    if status == 204:
        return "already"
    if status != 200:
        raise SimError("HEAD %s: %s" % (sha[:12], _describe(status, body.decode("utf-8", "replace"))))
    try:
        have = int(headers.get("x-cued-have", "0"))
    except ValueError:
        have = 0
    if have < 0 or have >= total:
        have = 0
    if have:
        log("  resuming %s at %d/%d" % (entry["key"], have, total))
    meta = json.dumps(entry, separators=(",", ":"))
    with open(path, "rb") as f:
        f.seek(have)
        pos = have
        first = True
        while pos < total:
            data = f.read(min(chunk_size, total - pos))
            if not data:
                raise SimError("%s changed while uploading" % path)
            end = pos + len(data) - 1
            hdrs = {"Content-Range": "bytes %d-%d/%d" % (pos, end, total), "Content-Type": entry.get("mime") or "application/octet-stream", **_bearer(token)}
            if first:
                hdrs["X-Cued-Meta"] = meta
                first = False
            status, _, body = request("PUT", url, data, hdrs, timeout=UPLOAD_TIMEOUT)
            if status == 201:
                return "uploaded"
            if status == 202:
                pos = end + 1
                continue
            if status == 422:
                raise SimError("desktop says the hash of %s did not match; the file may have changed" % entry["key"])
            raise SimError("PUT %s: %s" % (sha[:12], _describe(status, body.decode("utf-8", "replace"))))
    raise SimError("desktop never confirmed %s (sent every byte, no 201)" % entry["key"])


def sync_dir(base: str, token: str, root: str, chunk_size: int = CHUNK_SIZE, use_ffprobe: Optional[bool] = None,
             log: Callable[[str], None] = print) -> dict:
    root = os.path.abspath(root)
    entries = scan_dir(root, use_ffprobe)
    log("scanned %d track(s) in %s" % (len(entries), root))
    status, resp = request_json("POST", base + "/api/sync/manifest", {"tracks": entries}, token)
    if status != 200 or not isinstance(resp, dict):
        raise SimError("manifest refused: " + _describe(status, resp))
    missing = set(resp.get("missing") or [])
    known = resp.get("known")
    log("desktop knows %s track(s); %d to upload" % (known if known is not None else "?", len(missing)))
    by_sha = {e["sha256"]: e for e in entries}
    summary = {"scanned": len(entries), "known": known, "missing": len(missing), "uploaded": 0, "already": 0, "failed": 0, "bytes": 0}
    for sha in sorted(missing):
        e = by_sha.get(sha)
        if e is None:
            log("  desktop asked for %s which is not in the manifest; skipped" % sha[:12])
            continue
        path = os.path.join(root, *e["key"].split("/"))
        try:
            result = upload_track(base, token, e, path, chunk_size, log)
        except SimError as err:
            summary["failed"] += 1
            log("  FAILED %s: %s" % (e["key"], err))
            continue
        summary[result] += 1
        if result == "uploaded":
            summary["bytes"] += e["size"]
        log("  %s %s (%d bytes)" % (result, e["key"], e["size"]))
    log("done: %d uploaded, %d already there, %d failed, %d bytes sent" % (summary["uploaded"], summary["already"], summary["failed"], summary["bytes"]))
    return summary


def cmd_sync(args, state: dict) -> int:
    base, token = require_paired(state)
    if is_remote(base):
        raise SimError("uploads only on the LAN: %s is the funnel and the desktop refuses writes from outside (403); sync again on Wi-Fi" % base)
    use_ffprobe = False if args.no_ffprobe else None
    summary = sync_dir(base, token, args.dir, CHUNK_SIZE, use_ffprobe)
    state["last_sync"] = int(time.time())
    save_state(args.state, state)
    return 1 if summary["failed"] else 0


# ---------------------------------------------------------------- receive


def safe_filename(s: str) -> str:
    s = re.sub(r'[\\/:*?"<>|\x00-\x1f]', "_", s).strip(" .")
    return s[:120] or "track"


def receive_share(payload: links.SharePayload, directory: str, log: Callable[[str], None] = print) -> str:
    """Pull ``f`` into ``directory`` the way the phone's Receive screen does; returns the file path."""
    if not payload.file_url:
        raise SimError("share has no file url (f); the phone would queue the source link %s instead" % (payload.link or "-"))
    os.makedirs(directory, exist_ok=True)
    status, headers, body = request("GET", payload.file_url, timeout=UPLOAD_TIMEOUT)
    if status != 200:
        raise SimError("GET %s: %s" % (payload.file_url, _describe(status, body.decode("utf-8", "replace"))))
    ctype = headers.get("content-type", "").split(";")[0].strip().lower()
    ext = {"audio/mpeg": ".mp3", "audio/mp4": ".m4a", "audio/flac": ".flac", "audio/ogg": ".ogg", "audio/wav": ".wav", "audio/x-wav": ".wav"}.get(ctype)
    if ext is None:
        url_ext = os.path.splitext(urlsplit(payload.file_url).path)[1].lower()
        ext = url_ext if url_ext in AUDIO_EXT else ".mp3"
    name = safe_filename("%s - %s" % (payload.artist, payload.title) if payload.artist else payload.title) + ext
    dest = os.path.join(directory, name)
    with open(dest, "wb") as f:
        f.write(body)
    log("received %s (%d bytes, sha256 %s)" % (dest, len(body), hashlib.sha256(body).hexdigest()[:16]))
    if payload.genres or payload.bpm:
        log("  genres=%s bpm=%s link=%s" % (",".join(payload.genres) or "-", payload.bpm if payload.bpm is not None else "-", payload.link or "-"))
    if payload.friend_pubkey:
        log("  friend request from %s (%s): accept with  friends add %s" % (payload.friend_name or "someone", nostr.npub(payload.friend_pubkey), payload.friend_pubkey))
    return dest


def cmd_receive(args, state: dict) -> int:
    payload = links.SharePayload.decode(args.link)
    if payload is None:
        raise SimError("not a cued://share link")
    receive_share(payload, args.dir)
    return 0


# ---------------------------------------------------------------- station (websocket, optional dependency)


def _import_websockets():
    try:
        import websockets  # type: ignore
    except ImportError:
        raise MissingDependency("the station command needs the 'websockets' package:  python -m pip install websockets  (or pip install 'cued-companion[relay]')")
    return websockets


async def publish_station(relay: str, secret_hex: str, name: str, title: str, artist: str, seconds: float,
                          interval: float = 10.0, link: Optional[str] = None, log: Callable[[str], None] = print,
                          cert_sha256: Optional[str] = None) -> int:
    websockets = _import_websockets()
    started = int(time.time() * 1000)
    seq = 0
    published = 0
    deadline = time.monotonic() + seconds
    kw: dict = {}
    if relay.startswith("wss://") and cert_sha256:
        kw["ssl"] = pinned_ssl_context()
    try:
        async with websockets.connect(relay, open_timeout=CONNECT_TIMEOUT * 2, close_timeout=3, **kw) as ws:
            if "ssl" in kw:
                ssl_obj = ws.transport.get_extra_info("ssl_object")
                got = hashlib.sha256(ssl_obj.getpeercert(binary_form=True) or b"").hexdigest() if ssl_obj else ""
                if got != cert_sha256.lower():
                    raise NetError("relay %s: certificate pin mismatch (presented %s, QR said %s)" % (relay, got[:16], cert_sha256[:16]))
            while True:
                seq += 1
                content = json.dumps(nostr.station_state(name, title, artist, link=link, key="sim-1", started_at_ms=started, seq=seq), separators=(",", ":"))
                ev = nostr.sign_event(secret_hex, nostr.STATION_KIND, nostr.STATION_TAGS, content)
                await ws.send(nostr.event_message(ev))
                try:
                    reply = await asyncio.wait_for(ws.recv(), timeout=REQUEST_TIMEOUT)
                    verb, rest = nostr.parse_message(reply)
                    if verb == "OK" and len(rest) >= 2:
                        ok = bool(rest[1])
                        log("%s seq=%d id=%s… %s" % ("OK " if ok else "REJECTED", seq, ev["id"][:12], rest[2] if len(rest) > 2 and rest[2] else ""))
                        if ok:
                            published += 1
                    else:
                        log("relay said %s %s" % (verb, rest))
                except asyncio.TimeoutError:
                    log("no OK from relay for seq=%d" % seq)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                await asyncio.sleep(min(interval, remaining))
            seq += 1
            off = json.dumps(nostr.station_state(name, title, artist, status="off", started_at_ms=started, seq=seq, key="sim-1"), separators=(",", ":"))
            await ws.send(nostr.event_message(nostr.sign_event(secret_hex, nostr.STATION_KIND, nostr.STATION_TAGS, off)))
            log("went off air (seq=%d)" % seq)
    except (OSError, asyncio.TimeoutError) as e:
        raise NetError("relay %s: %s" % (relay, e))
    except Exception as e:  # websockets' own exception types, without importing them by name
        if e.__class__.__module__.startswith("websockets"):
            raise NetError("relay %s: %s" % (relay, e))
        raise
    return published


def cmd_station(args, state: dict) -> int:
    secret, pubkey = ensure_identity(state)
    save_state(args.state, state)
    relay = args.relay or (state.get("desktop_info") or {}).get("relay")
    if not relay:
        raise SimError("--relay is required (not paired, so no relay is known)")
    print("station %s by %s: publishing kind %d every %gs for %gs to %s" % (args.name, nostr.npub(pubkey), nostr.STATION_KIND, args.interval, args.seconds, relay))
    print("follow link: " + links.StationLink(pubkey, args.name, [relay]).encode())
    pin = getattr(args, "cert", None) or state.get("cert")
    n = asyncio.run(publish_station(relay, secret, args.name, args.title, args.artist, args.seconds, args.interval, args.link, cert_sha256=pin))
    print("%d state(s) accepted" % n)
    return 0 if n else 1


# ---------------------------------------------------------------- blobs, snapshot, status


def cmd_blob(args, state: dict) -> int:
    base, token = require_paired(state)
    if not BLOB_NAME.match(args.name):
        raise SimError("blob names are [A-Za-z0-9._-]{1,80}")
    url = "%s/api/backup/blob/%s" % (base, quote(args.name, safe=""))
    if args.op == "put":
        require_lan(base, "blob put")
        try:
            with open(args.file, "rb") as f:
                data = f.read()
        except OSError as e:
            raise SimError("cannot read %s: %s" % (args.file, e))
        status, _, body = request("PUT", url, data, {**_bearer(token), "Content-Type": "application/octet-stream"}, timeout=UPLOAD_TIMEOUT)
        if status not in (200, 201, 204):
            raise SimError("blob put: " + _describe(status, body.decode("utf-8", "replace")))
        print("stored %s (%d bytes)" % (args.name, len(data)))
    else:
        status, _, body = request("GET", url, headers=_bearer(token), timeout=UPLOAD_TIMEOUT)
        if status != 200:
            raise SimError("blob get: " + _describe(status, body.decode("utf-8", "replace")))
        with open(args.file, "wb") as f:
            f.write(body)
        print("fetched %s -> %s (%d bytes)" % (args.name, args.file, len(body)))
    return 0


def cmd_snapshot(args, state: dict) -> int:
    base, token = require_paired(state)
    require_lan(base, "snapshot")
    status, resp = request_json("POST", base + "/api/backup/snapshot", {}, token)
    if status not in (200, 201):
        raise SimError("snapshot: " + _describe(status, resp))
    print(json.dumps(resp, indent=2) if resp is not None else "snapshot taken")
    return 0


def cmd_status(args, state: dict) -> int:
    base, token = require_paired(state)
    status, resp = request_json("GET", base + "/api/sync/status", None, token)
    if status != 200:
        raise SimError("status: " + _describe(status, resp))
    print(json.dumps(resp, indent=2))
    return 0


# ---------------------------------------------------------------- account (§8), settings (§11)


def _get_json(base: str, token: str, path: str, what: str) -> object:
    status, resp = request_json("GET", base + path, None, token)
    if status != 200:
        raise SimError("%s: %s" % (what, _describe(status, resp)))
    return resp


def _print_json(obj) -> None:
    print(json.dumps(obj, indent=2, ensure_ascii=False) if obj is not None else "ok")


def fetch_chain(base: str, token: str) -> account_mod.Chain:
    """``GET /api/account/chain`` validated locally."""
    resp = _get_json(base, token, "/api/account/chain", "account chain")
    try:
        chain = account_mod.Chain.from_dict(resp)
        chain.validate()
    except account_mod.ChainError as e:
        raise SimError("the desktop's chain does not validate: %s" % e)
    return chain


def cmd_account(args, state: dict) -> int:
    base, token = require_paired(state)
    if args.op == "show":
        _print_json(_get_json(base, token, "/api/account", "account"))
        return 0
    if args.op == "chain":
        chain = fetch_chain(base, token)
        print("desktop chain: account %s (%s) height %d tip %s name %r devices %d" % (
            account_mod.account_id(chain.pubkey)[:16], nostr.npub(chain.pubkey), chain.height, chain.tip_hash[:16], chain.name(), len(chain.devices())))
        mine = load_chain(state)
        if mine is not None and mine.pubkey == chain.pubkey:
            if mine.merge(chain):
                store_chain(state, mine)
                save_state(args.state, state)
                print("merged into the sim's copy (now height %d)" % mine.height)
            else:
                print("the sim's copy is already current (height %d)" % mine.height)
        elif mine is not None:
            print("note: the sim holds a different account (%s); not merged" % nostr.npub(mine.pubkey))
        return 0
    if args.op == "push":
        require_lan(base, "account push")
        chain = load_chain(state)
        if chain is None:
            raise SimError("the sim has no account chain yet (pair with --account keep, or join)")
        status, resp = request_json("POST", base + "/api/account/chain", chain.to_dict(), token)
        if status not in (200, 201):
            raise SimError("account push: " + _describe(status, resp))
        print("pushed %d block(s), tip %s; desktop answered %s" % (chain.height, chain.tip_hash[:16], json.dumps(resp) if resp is not None else "ok"))
        return 0
    raise SimError("unknown account op %s" % args.op)


def cmd_settings(args, state: dict) -> int:
    base, token = require_paired(state)
    if args.op == "get":
        _print_json(_get_json(base, token, "/api/settings/shared", "settings"))
        return 0
    require_lan(base, "settings set")
    try:
        patch = json.loads(args.json)
    except ValueError as e:
        raise SimError("settings set wants a JSON object: %s" % e)
    if not isinstance(patch, dict):
        raise SimError("settings set wants a JSON object, e.g. '{\"relayEnabled\": true}'")
    status, resp = request_json("PUT", base + "/api/settings/shared", patch, token)
    if status not in (200, 201):
        raise SimError("settings set: " + _describe(status, resp))
    _print_json(resp)
    return 0


# ---------------------------------------------------------------- friends (§9), listening (§10), cover and lyrics


def _pubkey_arg(text: str) -> str:
    """A friend link, npub or 64-hex pubkey -> hex."""
    fl = links.FriendLink.decode(text)
    if fl is not None:
        return fl.pubkey
    parsed = nostr.nip19_parse(text)
    if parsed is None or parsed[0] == "nsec":
        raise SimError("not a pubkey: give 64 hex characters, an npub, or a cued://friend link")
    return parsed[1]


def cmd_friends(args, state: dict) -> int:
    base, token = require_paired(state)
    op = args.op
    if op == "list":
        _print_json(_get_json(base, token, "/api/friends", "friends"))
    elif op == "requests":
        _print_json(_get_json(base, token, "/api/friends/requests", "friend requests"))
    elif op == "link":
        resp = _get_json(base, token, "/api/friends/link", "friend link")
        if isinstance(resp, dict) and resp.get("link"):
            print(resp["link"])
        else:
            _print_json(resp)
    elif op == "add":
        if not args.arg:
            raise SimError("friends add wants a cued://friend link or an npub")
        require_lan(base, "friends add")
        status, resp = request_json("POST", base + "/api/friends", {"text": args.arg}, token)
        if status not in (200, 201):
            raise SimError("friends add: " + _describe(status, resp))
        _print_json(resp)
    elif op in ("accept", "remove"):
        if not args.arg:
            raise SimError("friends %s wants a pubkey" % op)
        require_lan(base, "friends " + op)
        pk = _pubkey_arg(args.arg)
        if op == "accept":
            status, resp = request_json("POST", "%s/api/friends/%s/accept" % (base, pk), {}, token)
        else:
            status, resp = request_json("DELETE", "%s/api/friends/%s" % (base, pk), None, token)
        if status not in (200, 204):
            raise SimError("friends %s: %s" % (op, _describe(status, resp)))
        _print_json(resp if resp is not None else {"pubkey": pk, "status": "accepted" if op == "accept" else "removed"})
    else:
        raise SimError("unknown friends op %s" % op)
    return 0


def cmd_listen(args, state: dict) -> int:
    base, token = require_paired(state)
    target = args.target
    if target == "status":
        _print_json(_get_json(base, token, "/api/stations/listen", "listen status"))
        return 0
    if target == "stop":
        require_lan(base, "listen stop")
        status, resp = request_json("DELETE", base + "/api/stations/listen", None, token)
        if status not in (200, 204):
            raise SimError("listen stop: " + _describe(status, resp))
        print("stopped listening")
        return 0
    require_lan(base, "listen")
    pk = _pubkey_arg(target)
    status, resp = request_json("POST", base + "/api/stations/listen", {"pubkey": pk}, token)
    if status not in (200, 201):
        raise SimError("listen: " + _describe(status, resp))
    print("listening to %s" % nostr.npub(pk))
    _print_json(resp)
    return 0


def _sha_arg(sha: str) -> str:
    sha = sha.strip().lower()
    if not links.is_hex(sha, 64):
        raise SimError("tracks are addressed by the 64-hex SHA-256 of the file")
    return sha


def cmd_lyrics(args, state: dict) -> int:
    base, token = require_paired(state)
    sha = _sha_arg(args.sha)
    status, _, body = request("GET", "%s/api/track/%s/lyrics" % (base, sha), headers=_bearer(token))
    if status == 404:
        raise SimError("no lyrics for %s…" % sha[:12])
    if status != 200:
        raise SimError("lyrics: " + _describe(status, body.decode("utf-8", "replace")))
    sys.stdout.write(body.decode("utf-8", "replace"))
    if body and not body.endswith(b"\n"):
        sys.stdout.write("\n")
    return 0


def cmd_cover(args, state: dict) -> int:
    base, token = require_paired(state)
    sha = _sha_arg(args.sha)
    status, headers, body = request("GET", "%s/api/track/%s/cover" % (base, sha), headers=_bearer(token), timeout=UPLOAD_TIMEOUT)
    if status == 404:
        raise SimError("no cover art for %s…" % sha[:12])
    if status != 200:
        raise SimError("cover: " + _describe(status, body.decode("utf-8", "replace")))
    try:
        with open(args.out, "wb") as f:
            f.write(body)
    except OSError as e:
        raise SimError("cannot write %s: %s" % (args.out, e))
    print("cover %s -> %s (%d bytes, %s)" % (sha[:12], args.out, len(body), headers.get("content-type", "?")))
    return 0


# ---------------------------------------------------------------- CLI


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="cued-phone-sim", description="A stand-in for the CUEd phone app, for testing the desktop.")
    ap.add_argument("--state", default=DEFAULT_STATE, help="where the token, device id and identity live (default %s)" % DEFAULT_STATE)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("pair", help="scan a cued://pair link and get a bearer token")
    p.add_argument("link")
    p.add_argument("--name", default="Sim Phone")
    p.add_argument("--account", choices=list(links.ACCOUNT_MODES), default="separate",
                   help="join: take the desktop's account; keep: the desktop takes this phone's; separate (default): two accounts")
    p.set_defaults(fn=cmd_pair, auth=False)

    p = sub.add_parser("sync", help="send a manifest of a music folder and upload what the desktop is missing")
    p.add_argument("dir")
    p.add_argument("--no-ffprobe", action="store_true", help="never run ffprobe; use the file name for artist/title")
    p.set_defaults(fn=cmd_sync)

    p = sub.add_parser("receive", help="download the file behind a cued://share link")
    p.add_argument("link")
    p.add_argument("--dir", default="received")
    p.set_defaults(fn=cmd_receive)

    p = sub.add_parser("station", help="publish signed kind-30777 station states to a relay (needs websockets)")
    p.add_argument("--relay", default=None, help="ws://host:8770/relay (default: the paired desktop's relay)")
    p.add_argument("--cert", default=None, help="SHA-256 of the desktop's funnel certificate for wss:// (default: the one from pairing)")
    p.add_argument("--name", required=True)
    p.add_argument("--title", required=True)
    p.add_argument("--artist", required=True)
    p.add_argument("--link", default=None, help="source link listeners can download from")
    p.add_argument("--seconds", type=float, default=30.0)
    p.add_argument("--interval", type=float, default=10.0)
    p.set_defaults(fn=cmd_station)

    p = sub.add_parser("blob", help="store or fetch an opaque backup blob")
    p.add_argument("op", choices=["put", "get"])
    p.add_argument("name")
    p.add_argument("file")
    p.set_defaults(fn=cmd_blob)

    p = sub.add_parser("snapshot", help="freeze the current manifest as a backup snapshot")
    p.set_defaults(fn=cmd_snapshot)

    p = sub.add_parser("status", help="GET /api/sync/status")
    p.set_defaults(fn=cmd_status)

    p = sub.add_parser("account", help="the account chain: show (GET /api/account), chain (GET + validate), push (POST the sim's chain)")
    p.add_argument("op", choices=["show", "chain", "push"])
    p.set_defaults(fn=cmd_account)

    p = sub.add_parser("settings", help="shared settings: get, or set '<json object>' (PUT /api/settings/shared)")
    p.add_argument("op", choices=["get", "set"])
    p.add_argument("json", nargs="?", default="{}")
    p.set_defaults(fn=cmd_settings)

    p = sub.add_parser("friends", help="list | requests | link | add <link-or-npub> | accept <pubkey> | remove <pubkey>")
    p.add_argument("op", choices=["list", "requests", "link", "add", "accept", "remove"])
    p.add_argument("arg", nargs="?", default=None)
    p.set_defaults(fn=cmd_friends)

    p = sub.add_parser("listen", help="listen in on a friend's station: <pubkey-or-npub> | stop | status")
    p.add_argument("target")
    p.set_defaults(fn=cmd_listen)

    p = sub.add_parser("lyrics", help="GET /api/track/<sha>/lyrics")
    p.add_argument("sha")
    p.set_defaults(fn=cmd_lyrics)

    p = sub.add_parser("cover", help="GET /api/track/<sha>/cover into a file")
    p.add_argument("sha")
    p.add_argument("out")
    p.set_defaults(fn=cmd_cover)
    return ap


# Commands that never talk to the paired desktop with the bearer, so they need no assertion.
NO_AUTH = {"pair", "receive", "station"}


def main(argv: Optional[List[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        state = load_state(args.state)
        global DEFAULT_PIN
        DEFAULT_PIN = state.get("cert")
        EXTRA_HEADERS.clear()
        if args.cmd not in NO_AUTH and state.get("desktop") and state.get("token") and is_remote(state["desktop"]):
            # Away from home: every request needs the biometric assertion (protocol.md §7).
            ensure_assertion(state)
            save_state(args.state, state)
        return args.fn(args, state)
    except SimError as e:
        print("error: %s" % e, file=sys.stderr)
        return e.code
    except KeyboardInterrupt:
        print("error: interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())

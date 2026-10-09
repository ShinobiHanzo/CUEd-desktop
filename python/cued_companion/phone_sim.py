#!/usr/bin/env python3
"""
A command-line stand-in for the phone, so the desktop (``cuedd``) can be
driven end to end without Android. Speaks exactly what ``docs/protocol.md``
says the phone speaks: ``cued://pair`` with the HMAC proof, the sync manifest,
resumable chunked uploads, backup blobs and snapshots, and signed kind-30777
station states over the relay websocket.

    cued-phone-sim pair "cued://pair?…" [--name "Sim Phone"]
    cued-phone-sim sync <dir>
    cued-phone-sim receive "cued://share?…" [--dir received]
    cued-phone-sim station --relay ws://host:8770/relay --name X --title T --artist A [--seconds 30]
    cued-phone-sim blob put <name> <file> | blob get <name> <file>
    cued-phone-sim snapshot
    cued-phone-sim status

State (token, device id, desktop URL, identity) lives in ``--state`` (default
``~/.cued-phone-sim.json``). Only the standard library is used; ``station``
needs the optional ``websockets`` package.

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

from . import links, nostr

DEFAULT_STATE = os.path.join("~", ".cued-phone-sim.json")
AUDIO_EXT = {".mp3", ".m4a", ".flac", ".ogg", ".opus", ".wav"}
MIME = {".mp3": "audio/mpeg", ".m4a": "audio/mp4", ".flac": "audio/flac", ".ogg": "audio/ogg", ".opus": "audio/ogg", ".wav": "audio/wav"}
CHUNK_SIZE = 1 << 20
CONNECT_TIMEOUT = 3.0
REQUEST_TIMEOUT = 30.0
UPLOAD_TIMEOUT = 120.0
BLOB_NAME = re.compile(r"^[A-Za-z0-9._-]{1,80}$")


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
        json.dump(state, f, indent=2, sort_keys=True)
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


def require_paired(state: dict) -> Tuple[str, str]:
    base, token = state.get("desktop"), state.get("token")
    if not base or not token:
        raise SimError("not paired yet: run  pair \"cued://pair?…\"  first")
    return base, token


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
        headers["Authorization"] = "Bearer " + token
    status, _, data = request(method, url, body, headers, timeout, opener)
    parsed: object = None
    if data:
        try:
            parsed = json.loads(data.decode("utf-8"))
        except ValueError:
            parsed = data.decode("utf-8", "replace")
    return status, parsed


def _bearer(token: str) -> Dict[str, str]:
    return {"Authorization": "Bearer " + token}


def _describe(status: int, body) -> str:
    if isinstance(body, dict) and body.get("error"):
        return "HTTP %d: %s" % (status, body["error"])
    if isinstance(body, str) and body.strip():
        return "HTTP %d: %s" % (status, body.strip()[:200])
    return "HTTP %d" % status


# ---------------------------------------------------------------- pair


def pair_with(link: links.PairLink, name: str, device: str, pubkey: str) -> Tuple[str, dict, Optional[str]]:
    """Try each LAN address, then the funnel with its cert pinned. Returns (base url used, response json, pin used)."""
    body = links.build_pair_request(link, name, device, pubkey)
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
            raise SimError("desktop refused the pairing (wrong or expired secret); show a fresh QR")
        if status == 409:
            raise SimError("this device id (%s) is already paired; unlink it on the desktop first" % device)
        errors.append("%s -> %s" % (base, _describe(status, resp)))
    raise NetError("no address answered: " + "; ".join(errors))


def cmd_pair(args, state: dict) -> int:
    link = links.PairLink.decode(args.link)
    if link is None:
        raise SimError("not a cued://pair link")
    device = ensure_device(state)
    _, pubkey = ensure_identity(state)
    base, resp, pin = pair_with(link, args.name, device, pubkey)
    state.update({"desktop": base, "token": resp["token"], "name": args.name, "desktop_info": resp.get("desktop", {}), "paired_at": int(time.time()), "cert": pin})
    global DEFAULT_PIN
    DEFAULT_PIN = pin
    save_state(args.state, state)
    info = resp.get("desktop", {})
    print("paired with %s at %s (relay %s, device %s)" % (info.get("name") or link.name or "desktop", base, info.get("relay") or link.relay or "-", device))
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


# ---------------------------------------------------------------- CLI


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="cued-phone-sim", description="A stand-in for the CUEd phone app, for testing the desktop.")
    ap.add_argument("--state", default=DEFAULT_STATE, help="where the token, device id and identity live (default %s)" % DEFAULT_STATE)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("pair", help="scan a cued://pair link and get a bearer token")
    p.add_argument("link")
    p.add_argument("--name", default="Sim Phone")
    p.set_defaults(fn=cmd_pair)

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
    return ap


def main(argv: Optional[List[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        state = load_state(args.state)
        global DEFAULT_PIN
        DEFAULT_PIN = state.get("cert")
        return args.fn(args, state)
    except SimError as e:
        print("error: %s" % e, file=sys.stderr)
        return e.code
    except KeyboardInterrupt:
        print("error: interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())

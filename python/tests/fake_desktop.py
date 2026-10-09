"""
An in-process stand-in for ``cuedd``'s phone-facing API, built on
``http.server`` and written straight from ``docs/protocol.md`` §2–3 and
§7–§11. It exists to test the phone simulator, and doubles as executable
documentation of what the desktop has to answer.

Implements:
    POST /api/pair                     HMAC proof check; 403 wrong secret, 409 device known;
                                       records biokey/devhash; account join -> {"account":{"sealed"}}
    POST /api/sync/manifest            bearer; -> {"missing": [...], "known": n}
    HEAD /api/sync/track/{sha}         200 + X-Cued-Have, or 204 when complete
    PUT  /api/sync/track/{sha}         Content-Range chunks; 201 / 202 / 422
    GET  /api/sync/status
    POST /api/backup/snapshot, GET /api/backup/snapshots
    PUT/GET /api/backup/blob/{name}, GET /api/backup/blobs
    GET  /api/library
    GET  /api/track/{sha}/lyrics, /cover
    GET  /share/{sha}/{cap}            a share file the Receive screen can pull (no bearer)
    GET  /api/auth/challenge           {"challenge","expiresInSec"}
    GET  /api/account, GET/POST /api/account/chain, POST /api/account/import
    GET/PUT /api/settings/shared
    GET  /api/friends, /api/friends/requests, /api/friends/link; POST /api/friends;
         POST /api/friends/{pk}/accept; DELETE /api/friends/{pk}
    GET/POST/DELETE /api/stations/listen

Set ``desktop.remote = True`` and every request is treated as coming through
the funnel (§7): a valid ``X-Cued-Assertion`` is required and anything but
GET/HEAD is refused with 403 before a handler runs.
"""
import base64
import hashlib
import json
import os
import re
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from cued_companion import account as account_mod
from cued_companion import bio, links, nostr, seal

BLOB_NAME = re.compile(r"^[A-Za-z0-9._-]{1,80}$")
ASSERTION_TTL = 600
DEFAULT_SETTINGS = {"relayEnabled": False, "friendStreaming": True,
                    "theme": {"bg": "#0e0f13", "panel": "#161820", "text": "#ececf1", "accent": "#5ef2c6", "accent2": "#7aa2ff", "danger": "#ff6b7a"},
                    "stationName": ""}
# Read-only list from §7: what a remote phone may GET/HEAD.
REMOTE_READS = re.compile(r"^/api/(me|library|track/[0-9a-f]{64}(/meta|/cover|/lyrics)?|sync/status|backup/snapshots|backup/blobs|backup/blob/[^/]+"
                          r"|stations/feed|stations/follows|stations/listen|friends|friends/requests|friends/link|account|account/chain|settings/shared|auth/challenge)$")


class FakeDesktop(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, name="Fake desktop", secret=None):
        super().__init__(("127.0.0.1", 0), FakeHandler)
        self.name = name
        self.secret = secret or nostr.generate_secret().hex()   # the desktop's own identity (§4)
        self.chain = None          # account chain once the desktop has an account (§8); None = never had one
        self.secrets = {}          # pending pairing secrets -> expiry
        self.devices = {}          # device id -> {"token", "name", "pubkey", "biokey", "devhash", "secret", "manifest", "manifest_at", "upload_at"}
        self.tokens = {}           # token -> device id
        self.complete = {}         # sha -> {"bytes", "meta", "devices"}
        self.partial = {}          # sha -> {"bytes": bytearray, "meta"}
        self.blobs = {}            # (device, name) -> bytes
        self.snapshots = {}        # device -> [iso time]
        self.shares = {}           # (sha, cap) -> bytes
        self.lyrics = {}           # sha -> text
        self.covers = {}           # sha -> (mime, bytes)
        self.challenges = {}       # challenge -> expiry
        self.friends = {}          # pubkey -> {"pubkey", "name", "status"}  (pending_out / pending_in / friends)
        self.listening = None      # pubkey or None
        self.remote = False        # treat every request as Origin::External (§7)
        self.lock = threading.Lock()
        self.requests = []         # (method, path) log for assertions
        self.assertions = []       # (path, header) every X-Cued-Assertion seen
        self.settings_log = []     # every PUT /api/settings/shared patch

    @property
    def pubkey(self):
        return nostr.public_key(bytes.fromhex(self.secret)).hex()

    def ensure_account(self, ts=None):
        """A desktop that never had an account makes one on the spot (§8)."""
        if self.chain is None:
            ts = ts or int(time.time())
            self.chain = account_mod.Chain.create(self.secret, self.name, ts)
            self.chain.append(self.secret, "bind_device", {"devhash": account_mod.devhash("desktop", "fake"), "name": self.name, "kind": "desktop"}, ts)
        return self.chain

    def shared_settings(self):
        out = json.loads(json.dumps(DEFAULT_SETTINGS))
        if self.chain is not None:
            out.update(self.chain.settings())
        return out

    def friend_link(self):
        return links.FriendLink(self.pubkey, self.name, [self.base.replace("http", "ws") + "/relay"]).encode()

    @property
    def base(self):
        return "http://127.0.0.1:%d" % self.server_address[1]

    def new_pair_link(self, funnel=None, cert=None):
        secret = links.new_pair_secret()
        self.secrets[secret] = time.time() + 600
        return links.PairLink(secret=secret, name=self.name, addresses=[self.base], funnel=funnel, cert_sha256=cert,
                              pubkey=self.pubkey, relay=self.base.replace("http", "ws") + "/relay")

    def start(self):
        threading.Thread(target=self.serve_forever, daemon=True).start()
        return self

    def stop(self):
        self.shutdown()
        self.server_close()


class FakeHandler(BaseHTTPRequestHandler):
    server: FakeDesktop
    protocol_version = "HTTP/1.1"

    # -- plumbing

    def _send(self, code, body=b"", headers=None, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD" and body:
            self.wfile.write(body)

    def _json(self, code, obj, headers=None):
        self._send(code, json.dumps(obj).encode(), headers)

    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def _device(self):
        auth = self.headers.get("Authorization", "")
        if not auth.startswith("Bearer "):
            return None
        return self.server.tokens.get(auth[7:].strip())

    def log_message(self, *a):
        pass

    def _tier_check(self, path):
        """§7: through the funnel only GET/HEAD on the read-only list, with a valid assertion. Returns an error code or None."""
        hdr = self.headers.get("X-Cued-Assertion")
        if hdr:
            self.server.assertions.append((path, hdr))
        if not self.server.remote or path == "/api/pair" or path.startswith("/share/"):
            return None
        if self.command not in ("GET", "HEAD") or not REMOTE_READS.match(path):
            return self._json(403, {"error": "writes only on the LAN"})
        dev = self._device()
        if dev is None:
            return self._json(401, {"error": "bearer required"})
        if path == "/api/auth/challenge":
            return None
        info = self.server.devices[dev]
        if not info.get("biokey"):
            return self._json(403, {"error": "no biokey registered; remote reads need one"})
        if not hdr or "." not in hdr:
            return self._json(403, {"error": "assertion required"})
        challenge = hdr.split(".", 1)[0]
        with self.server.lock:
            exp = self.server.challenges.get(challenge)
        if exp is None or exp < time.time():
            return self._json(403, {"error": "unknown or expired challenge"})
        if not bio.verify_assertion(info["biokey"], dev, hdr):
            return self._json(403, {"error": "bad assertion"})
        return None

    def _route(self):
        self.server.requests.append((self.command, self.path))
        path = self.path.split("?", 1)[0]
        try:
            if self._tier_check(path) is not None:
                return
            handler = getattr(self, "h_" + self.command.lower())
            handler(path)
        except Exception as e:  # noqa: BLE001  (a test server should never hang the client)
            self._json(500, {"error": repr(e)})

    do_GET = do_POST = do_PUT = do_HEAD = do_DELETE = _route

    # -- pairing (§2)

    def h_post(self, path):
        if path == "/api/pair":
            return self._pair()
        dev = self._device()
        if dev is None:
            return self._json(401, {"error": "bearer required"})
        if path == "/api/sync/manifest":
            return self._manifest(dev)
        if path == "/api/backup/snapshot":
            iso = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
            self.server.snapshots.setdefault(dev, []).append(iso)
            return self._json(200, {"snapshot": iso, "tracks": len(self.server.devices[dev]["manifest"])})
        if path == "/api/account/import":
            return self._account_import(dev)
        if path == "/api/account/chain":
            return self._account_merge()
        if path == "/api/friends":
            return self._friend_add()
        m = re.match(r"^/api/friends/([0-9a-f]{64})/accept$", path)
        if m:
            f = self.server.friends.get(m.group(1))
            if f is None or f["status"] != "pending_in":
                return self._json(404, {"error": "no pending request from that key"})
            f["status"] = "friends"
            return self._json(200, f)
        if path == "/api/stations/listen":
            try:
                pk = json.loads(self._body())["pubkey"]
            except (ValueError, KeyError, TypeError):
                return self._json(400, {"error": "pubkey required"})
            if not links.is_hex(pk, 64):
                return self._json(400, {"error": "bad pubkey"})
            self.server.listening = pk
            return self._json(200, {"listening": {"pubkey": pk, "name": (self.server.friends.get(pk) or {}).get("name", ""), "now": None, "next": []}})
        self._json(404, {"error": "not found"})

    # -- accounts (§8)

    def _account_import(self, dev):
        try:
            sealed = json.loads(self._body())["sealed"]
        except (ValueError, KeyError, TypeError):
            return self._json(400, {"error": "sealed required"})
        info = self.server.devices[dev]
        try:
            raw = seal.open_sealed(bytes.fromhex(info["secret"]), "account", sealed)
            bundle = json.loads(raw.decode("utf-8"))
            chain = account_mod.Chain.from_dict(bundle["chain"])
            chain.validate()
            secret = bundle["secret_hex"]
            if nostr.public_key(bytes.fromhex(secret)).hex() != chain.pubkey:
                raise ValueError("key does not match chain")
        except (seal.SealError, ValueError, KeyError, TypeError, account_mod.ChainError) as e:
            return self._json(400, {"error": "bad bundle: %s" % e})
        with self.server.lock:
            self.server.secret = secret
            chain.append(secret, "bind_device", {"devhash": account_mod.devhash("desktop", "fake"), "name": self.server.name, "kind": "desktop"}, int(time.time()))
            self.server.chain = chain
        self._json(200, {"id": account_mod.account_id(chain.pubkey), "pubkey": chain.pubkey, "height": chain.height})

    def _account_merge(self):
        try:
            other = account_mod.Chain.from_dict(json.loads(self._body()))
            other.validate()
        except (ValueError, account_mod.ChainError) as e:
            return self._json(400, {"error": "bad chain: %s" % e})
        with self.server.lock:
            if self.server.chain is None:
                if other.pubkey != self.server.pubkey:
                    return self._json(409, {"error": "different account"})
                self.server.chain = other
                changed = True
            else:
                try:
                    changed = self.server.chain.merge(other)
                except account_mod.ChainError as e:
                    return self._json(409, {"error": str(e)})
            return self._json(200, {"changed": changed, "height": self.server.chain.height, "tip": self.server.chain.tip_hash})

    # -- friends (§9)

    def _friend_add(self):
        try:
            text = json.loads(self._body())["text"]
        except (ValueError, KeyError, TypeError):
            return self._json(400, {"error": "text required"})
        fl = links.FriendLink.decode(text)
        if fl is not None:
            pk, name = fl.pubkey, fl.name
        else:
            parsed = nostr.nip19_parse(text)
            if parsed is None or parsed[0] == "nsec":
                return self._json(400, {"error": "not a friend link or npub"})
            pk, name = parsed[1], ""
        f = self.server.friends.get(pk)
        if f is None:
            f = {"pubkey": pk, "name": name, "status": "pending_out"}
            self.server.friends[pk] = f
        elif f["status"] == "pending_in":
            f["status"] = "friends"   # both scanned each other
        if name:
            f["name"] = name
        self._json(201, f)

    def _pair(self):
        try:
            req = json.loads(self._body())
        except ValueError:
            return self._json(400, {"error": "bad json"})
        for k in ("name", "device", "nonce", "proof"):
            if not isinstance(req.get(k), str):
                return self._json(400, {"error": "missing " + k})
        now = time.time()
        with self.server.lock:
            match = None
            for secret, expiry in list(self.server.secrets.items()):
                if expiry < now:
                    del self.server.secrets[secret]
                    continue
                if links.verify_pair_proof(secret, req["device"], req["name"], req["nonce"], req["proof"]):
                    match = secret
                    break
            if match is None:
                return self._json(403, {"error": "wrong or expired secret"})
            if req["device"] in self.server.devices:
                return self._json(409, {"error": "device already paired"})
            del self.server.secrets[match]  # one secret pairs one phone
            mode = req.get("account", "separate")
            if mode not in links.ACCOUNT_MODES:
                return self._json(400, {"error": "bad account mode"})
            biokey = req.get("biokey")
            if biokey is not None and not bio.valid_biokey(biokey):
                return self._json(400, {"error": "bad biokey"})
            devhash = req.get("devhash")
            if devhash is not None and not links.is_hex(devhash, 64):
                return self._json(400, {"error": "bad devhash"})
            token = base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip("=")
            self.server.devices[req["device"]] = {"token": token, "name": req["name"], "pubkey": req.get("pubkey", ""), "biokey": biokey,
                                                  "devhash": devhash, "secret": match, "account": mode,
                                                  "manifest": [], "manifest_at": None, "upload_at": None}
            self.server.tokens[token] = req["device"]
            out = {"token": token, "desktop": {"name": self.server.name, "pubkey": self.server.pubkey,
                                               "relay": self.server.base.replace("http", "ws") + "/relay", "funnel": None, "cert": None}}
            if mode == "join":
                chain = self.server.ensure_account()
                if devhash:
                    chain.append(self.server.secret, "bind_device", {"devhash": devhash, "name": req["name"], "kind": "phone"}, int(time.time()))
                bundle = json.dumps({"secret_hex": self.server.secret, "chain": chain.to_dict()}, separators=(",", ":")).encode()
                out["account"] = {"sealed": seal.seal(bytes.fromhex(match), "account", bundle)}
        self._json(200, out)

    # -- manifest (§3)

    def _manifest(self, dev):
        try:
            tracks = json.loads(self._body())["tracks"]
        except (ValueError, KeyError, TypeError):
            return self._json(400, {"error": "bad manifest"})
        with self.server.lock:
            self.server.devices[dev]["manifest"] = tracks
            self.server.devices[dev]["manifest_at"] = int(time.time() * 1000)
            missing = [t["sha256"] for t in tracks if t["sha256"] not in self.server.complete]
            for t in tracks:
                if t["sha256"] in self.server.complete:
                    self.server.complete[t["sha256"]]["devices"].add(dev)
            self._json(200, {"missing": missing, "known": len(self.server.complete)})

    # -- upload (§3), resumable

    def h_head(self, path):
        dev = self._device()
        if dev is None:
            return self._json(401, {"error": "bearer required"})
        m = re.match(r"^/api/sync/track/([0-9a-f]{64})$", path)
        if not m:
            return self._send(404)
        sha = m.group(1)
        with self.server.lock:
            if sha in self.server.complete:
                return self._send(204)
            have = len(self.server.partial.get(sha, {}).get("bytes", b""))
        self._send(200, headers={"X-Cued-Have": str(have)})

    def h_put(self, path):
        dev = self._device()
        if dev is None:
            return self._json(401, {"error": "bearer required"})
        m = re.match(r"^/api/sync/track/([0-9a-f]{64})$", path)
        if m:
            return self._put_track(dev, m.group(1))
        m = re.match(r"^/api/backup/blob/([^/]+)$", path)
        if m:
            name = m.group(1)
            if not BLOB_NAME.match(name):
                return self._json(400, {"error": "bad blob name"})
            self.server.blobs[(dev, name)] = self._body()
            return self._json(201, {"name": name, "size": len(self.server.blobs[(dev, name)])})
        if path == "/api/settings/shared":
            try:
                patch = json.loads(self._body())
            except ValueError:
                return self._json(400, {"error": "bad json"})
            if not isinstance(patch, dict):
                return self._json(400, {"error": "settings must be an object"})
            with self.server.lock:
                chain = self.server.ensure_account()
                chain.append(self.server.secret, "settings", patch, int(time.time()))
                self.server.settings_log.append(patch)
            return self._json(200, self.server.shared_settings())
        self._json(404, {"error": "not found"})

    def h_delete(self, path):
        dev = self._device()
        if dev is None:
            return self._json(401, {"error": "bearer required"})
        m = re.match(r"^/api/friends/([0-9a-f]{64})$", path)
        if m:
            if self.server.friends.pop(m.group(1), None) is None:
                return self._json(404, {"error": "not a friend"})
            return self._json(200, {"pubkey": m.group(1), "status": "removed"})
        if path == "/api/stations/listen":
            self.server.listening = None
            return self._send(204)
        self._json(404, {"error": "not found"})

    def _put_track(self, dev, sha):
        data = self._body()
        crange = self.headers.get("Content-Range")
        meta_hdr = self.headers.get("X-Cued-Meta")
        with self.server.lock:
            if sha in self.server.complete:
                return self._json(201, {"sha256": sha, "note": "already complete"})
            if crange:
                m = re.match(r"^bytes (\d+)-(\d+)/(\d+)$", crange)
                if not m:
                    return self._json(400, {"error": "bad Content-Range"})
                start, end, total = int(m.group(1)), int(m.group(2)), int(m.group(3))
                if end - start + 1 != len(data):
                    return self._json(400, {"error": "Content-Range does not match body"})
                part = self.server.partial.get(sha)
                if start == 0 or part is None:
                    part = {"bytes": bytearray(), "meta": None, "total": total}
                    self.server.partial[sha] = part
                if len(part["bytes"]) != start:
                    return self._json(409, {"error": "expected offset %d" % len(part["bytes"])})
                part["total"] = total
            else:
                part = {"bytes": bytearray(), "meta": None, "total": None}
                self.server.partial[sha] = part
            if meta_hdr:
                try:
                    part["meta"] = json.loads(meta_hdr)
                except ValueError:
                    return self._json(400, {"error": "bad X-Cued-Meta"})
            part["bytes"] += data
            total = part["total"]
            if total is None and part["meta"]:
                total = part["meta"].get("size")
            if total is not None and len(part["bytes"]) < total:
                return self._json(202, {"have": len(part["bytes"])})
            # complete: check the hash
            blob = bytes(part["bytes"])
            del self.server.partial[sha]
            if hashlib.sha256(blob).hexdigest() != sha:
                return self._json(422, {"error": "sha256 mismatch"})
            self.server.complete[sha] = {"bytes": blob, "meta": part["meta"] or {}, "devices": {dev}}
            self.server.devices[dev]["upload_at"] = int(time.time() * 1000)
            return self._json(201, {"sha256": sha, "size": len(blob)})

    # -- reads

    def h_get(self, path):
        m = re.match(r"^/share/([0-9a-f]{64})/([A-Za-z0-9_-]+)$", path)
        if m:
            data = self.server.shares.get((m.group(1), m.group(2)))
            if data is None:
                return self._json(404, {"error": "no such share"})
            return self._send(200, data, ctype="audio/wav")
        dev = self._device()
        if dev is None:
            return self._json(401, {"error": "bearer required"})
        if path == "/api/sync/status":
            out = {}
            for d, info in self.server.devices.items():
                backed = [t for t in info["manifest"] if t["sha256"] in self.server.complete]
                out[d] = {"name": info["name"], "tracksOnPhone": len(info["manifest"]), "tracksBackedUp": len(backed),
                          "bytes": sum(t.get("size", 0) for t in backed), "lastManifest": info["manifest_at"], "lastUpload": info["upload_at"]}
            return self._json(200, {"devices": out})
        if path == "/api/library":
            tracks = [dict(v["meta"], sha256=sha, devices=sorted(v["devices"])) for sha, v in self.server.complete.items()]
            return self._json(200, {"tracks": tracks, "total": len(tracks)})
        if path == "/api/auth/challenge":
            ch = os.urandom(16).hex()
            with self.server.lock:
                self.server.challenges[ch] = time.time() + 120
            return self._json(200, {"challenge": ch, "expiresInSec": 120})
        if path == "/api/account":
            chain = self.server.chain
            return self._json(200, {"id": account_mod.account_id(self.server.pubkey), "pubkey": self.server.pubkey, "npub": nostr.npub(self.server.pubkey),
                                    "name": chain.name() if chain else self.server.name, "height": chain.height if chain else 0,
                                    "devices": chain.devices() if chain else []})
        if path == "/api/account/chain":
            chain = self.server.ensure_account()
            return self._json(200, chain.to_dict())
        if path == "/api/settings/shared":
            return self._json(200, self.server.shared_settings())
        if path == "/api/friends":
            return self._json(200, {"friends": [f for f in self.server.friends.values() if f["status"] == "friends"]})
        if path == "/api/friends/requests":
            return self._json(200, {"requests": [f for f in self.server.friends.values() if f["status"] != "friends"]})
        if path == "/api/friends/link":
            return self._json(200, {"link": self.server.friend_link(), "svg": "<svg/>"})
        if path == "/api/stations/listen":
            pk = self.server.listening
            return self._json(200, {"listening": None if pk is None else {"pubkey": pk, "name": (self.server.friends.get(pk) or {}).get("name", ""), "now": None, "next": []}})
        m = re.match(r"^/api/track/([0-9a-f]{64})/lyrics$", path)
        if m:
            text = self.server.lyrics.get(m.group(1))
            if text is None:
                return self._json(404, {"error": "no lyrics"})
            return self._send(200, text.encode("utf-8"), ctype="text/plain; charset=utf-8")
        m = re.match(r"^/api/track/([0-9a-f]{64})/cover$", path)
        if m:
            cover = self.server.covers.get(m.group(1))
            if cover is None:
                return self._json(404, {"error": "no cover"})
            return self._send(200, cover[1], ctype=cover[0])
        if path == "/api/backup/snapshots":
            return self._json(200, {"snapshots": self.server.snapshots.get(dev, [])})
        if path == "/api/backup/blobs":
            return self._json(200, {"blobs": sorted(n for (d, n) in self.server.blobs if d == dev)})
        m = re.match(r"^/api/backup/blob/([^/]+)$", path)
        if m:
            data = self.server.blobs.get((dev, m.group(1)))
            if data is None:
                return self._json(404, {"error": "no such blob"})
            return self._send(200, data, ctype="application/octet-stream")
        self._json(404, {"error": "not found"})

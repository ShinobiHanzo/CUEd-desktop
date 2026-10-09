"""
An in-process stand-in for ``cuedd``'s phone-facing API, built on
``http.server`` and written straight from ``docs/protocol.md`` §2–3. It exists
to test the phone simulator, and doubles as executable documentation of what
the desktop has to answer.

Implements:
    POST /api/pair                     HMAC proof check; 403 wrong secret, 409 device known
    POST /api/sync/manifest            bearer; -> {"missing": [...], "known": n}
    HEAD /api/sync/track/{sha}         200 + X-Cued-Have, or 204 when complete
    PUT  /api/sync/track/{sha}         Content-Range chunks; 201 / 202 / 422
    GET  /api/sync/status
    POST /api/backup/snapshot, GET /api/backup/snapshots
    PUT/GET /api/backup/blob/{name}, GET /api/backup/blobs
    GET  /api/library
    GET  /share/{sha}/{cap}            a share file the Receive screen can pull (no bearer)
"""
import base64
import hashlib
import json
import os
import re
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from cued_companion import links

BLOB_NAME = re.compile(r"^[A-Za-z0-9._-]{1,80}$")


class FakeDesktop(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, name="Fake desktop", pubkey="3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"):
        super().__init__(("127.0.0.1", 0), FakeHandler)
        self.name = name
        self.pubkey = pubkey
        self.secrets = {}          # pending pairing secrets -> expiry
        self.devices = {}          # device id -> {"token", "name", "pubkey", "manifest", "manifest_at", "upload_at"}
        self.tokens = {}           # token -> device id
        self.complete = {}         # sha -> {"bytes", "meta", "devices"}
        self.partial = {}          # sha -> {"bytes": bytearray, "meta"}
        self.blobs = {}            # (device, name) -> bytes
        self.snapshots = {}        # device -> [iso time]
        self.shares = {}           # (sha, cap) -> bytes
        self.lock = threading.Lock()
        self.requests = []         # (method, path) log for assertions

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

    def _route(self):
        self.server.requests.append((self.command, self.path))
        path = self.path.split("?", 1)[0]
        try:
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
        self._json(404, {"error": "not found"})

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
            token = base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip("=")
            self.server.devices[req["device"]] = {"token": token, "name": req["name"], "pubkey": req.get("pubkey", ""),
                                                  "manifest": [], "manifest_at": None, "upload_at": None}
            self.server.tokens[token] = req["device"]
        self._json(200, {"token": token, "desktop": {"name": self.server.name, "pubkey": self.server.pubkey,
                                                     "relay": self.server.base.replace("http", "ws") + "/relay", "funnel": None, "cert": None}})

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

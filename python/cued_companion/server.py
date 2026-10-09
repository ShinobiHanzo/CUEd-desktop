#!/usr/bin/env python3
"""
CUEd companion download worker.

A small wrapper around ``spotdl`` so a phone on the same Wi-Fi can ask a
laptop / Raspberry Pi / old PC to do the downloading. Standalone it is the
same program as CUEd's ``tools/spotdl-server``; inside the desktop, ``cuedd``
starts it bound to loopback and proxies the phone's requests to it.

Usage:
    python -m cued_companion.server [--port 8766] [--dir ./downloads] [--bind 0.0.0.0]

API (JSON):
    GET    /health            -> "ok"
    GET    /                  -> status page with the addresses to enter in the app
    POST   /jobs {url, format, lrc} -> {"id": "..."}   (lrc: also write .lrc lyrics files)
    GET    /jobs              -> {"jobs": [{"id", "status", "progress", "message", "files", ...}]}
    GET    /jobs/<id>         -> {"status": "queued|running|done|failed", "progress": 0..1, "message": "...", "files": [...]}
    DELETE /jobs/<id>         -> {"ok": true}   (only once the job is done or failed; 409 otherwise)
    GET    /files/<name>      -> the audio (or .lrc) file

Security model: no auth. It only serves files it downloaded itself and only
accepts URLs (or search strings) for spotdl; anything starting with ``-`` is
refused so a request can never smuggle a spotdl flag. Don't expose it to the
internet.

Testing: ``create_server(..., runner=fake)`` swaps the spotdl subprocess for
any callable ``runner(job, download_dir, emit) -> exit code``.
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Callable, Deque, Dict, List, Optional
from urllib.parse import unquote, urlsplit

AUDIO_EXT = {".mp3", ".m4a", ".opus", ".ogg", ".flac", ".wav"}
TEXT_EXT = {".lrc"}
SERVE_EXT = AUDIO_EXT | TEXT_EXT
FORMATS = {"mp3", "m4a", "opus", "flac", "ogg", "wav"}
MIME = {
    ".m4a": "audio/mp4",
    ".opus": "audio/ogg",
    ".ogg": "audio/ogg",
    ".flac": "audio/flac",
    ".wav": "audio/wav",
    ".lrc": "text/plain; charset=utf-8",
}
DEFAULT_PORT = 8766

Runner = Callable[[dict, str, Callable[[str], None]], int]


def log(*a) -> None:
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def lan_ips() -> List[str]:
    """Best-effort list of this machine's LAN addresses (what to type into the app)."""
    ips = set()
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            s.connect(("10.255.255.255", 1))
            ips.add(s.getsockname()[0])
        finally:
            s.close()
    except OSError:
        pass
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            ip = info[4][0]
            if not ip.startswith("127."):
                ips.add(ip)
    except OSError:
        pass
    return sorted(ips)


FFMPEG_ARGS: List[str] = []


def ensure_ffmpeg() -> bool:
    """spotdl needs ffmpeg. Use the one on PATH, else let spotdl download its own copy (Windows/Linux/macOS)."""
    if shutil.which("ffmpeg"):
        return True
    log("ffmpeg not on PATH; asking spotdl to download a private copy (one-time)…")
    try:
        r = subprocess.run([sys.executable, "-m", "spotdl", "--download-ffmpeg"], input=b"y\n", capture_output=True, timeout=600)
    except (OSError, subprocess.SubprocessError) as e:
        log("could not get ffmpeg:", e)
        return False
    if r.returncode == 0:
        log("ffmpeg downloaded by spotdl")
        return True
    log("could not get ffmpeg:", r.stderr.decode(errors="replace")[-400:])
    return False


def snapshot(directory: str) -> set:
    try:
        return {f for f in os.listdir(directory) if os.path.splitext(f)[1].lower() in SERVE_EXT}
    except OSError:
        return set()


def build_command(job: dict, download_dir: str) -> List[str]:
    cmd = [
        sys.executable, "-m", "spotdl", "download", job["url"],
        "--output", os.path.join(download_dir, "{artists} - {title}.{output-ext}"),
        "--format", job.get("format", "mp3"),
        "--overwrite", "skip",
        "--simple-tui",
    ]
    if job.get("lrc"):
        cmd.append("--generate-lrc")
    cmd += FFMPEG_ARGS
    return cmd


def spotdl_runner(job: dict, download_dir: str, emit: Callable[[str], None]) -> int:
    """The real thing: run spotdl, feed every output line to ``emit``, return its exit code."""
    cmd = build_command(job, download_dir)
    log("running:", " ".join(cmd))
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
    assert proc.stdout is not None
    for line in proc.stdout:
        emit(line)
    return proc.wait()


def progress_from_line(line: str) -> Optional[float]:
    """spotdl prints a percentage in its simple tui; map the first 1..100 integer token to 0.05..0.90."""
    for tok in line.replace("%", " % ").split():
        if tok.isdigit() and 0 < int(tok) <= 100:
            return 0.05 + 0.85 * int(tok) / 100
    return None


PUBLIC_KEYS = ("id", "url", "format", "lrc", "status", "progress", "message", "files", "created")


class CompanionServer(ThreadingHTTPServer):
    """One download queue, one worker thread, served over HTTP."""

    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address, download_dir: str, runner: Optional[Runner] = None):
        self.download_dir = os.path.abspath(download_dir)
        os.makedirs(self.download_dir, exist_ok=True)
        self.runner: Runner = runner or spotdl_runner
        self.jobs: Dict[str, dict] = {}
        self.lock = threading.Lock()
        self.queue: Deque[str] = collections.deque()
        self.wake = threading.Event()
        self.stopping = threading.Event()
        self.current: Optional[str] = None
        super().__init__(address, Handler)
        self.worker = threading.Thread(target=self._worker, name="cued-companion-worker", daemon=True)
        self.worker.start()

    # -- lifecycle

    def server_close(self) -> None:
        self.stopping.set()
        self.wake.set()
        super().server_close()

    # -- jobs

    def submit(self, url: str, fmt: str = "mp3", lrc: bool = False) -> str:
        job_id = uuid.uuid4().hex[:12]
        job = {
            "id": job_id, "url": url, "format": fmt, "lrc": bool(lrc),
            "status": "queued", "progress": 0.0, "message": "queued", "files": [],
            "created": time.time(),
        }
        with self.lock:
            self.jobs[job_id] = job
            self.queue.append(job_id)
        self.wake.set()
        log("queued", job_id, url)
        return job_id

    def job_view(self, job: dict) -> dict:
        return {k: job[k] for k in PUBLIC_KEYS if k in job}

    def list_jobs(self) -> List[dict]:
        with self.lock:
            return [self.job_view(j) for j in self.jobs.values()]

    def get_job(self, job_id: str) -> Optional[dict]:
        with self.lock:
            job = self.jobs.get(job_id)
            return self.job_view(job) if job else None

    def delete_job(self, job_id: str) -> int:
        """HTTP status: 200 removed, 404 unknown, 409 still queued or running."""
        with self.lock:
            job = self.jobs.get(job_id)
            if job is None:
                return 404
            if job["status"] not in ("done", "failed"):
                return 409
            del self.jobs[job_id]
            return 200

    def _worker(self) -> None:
        while not self.stopping.is_set():
            with self.lock:
                job_id = self.queue.popleft() if self.queue else None
            if job_id is None:
                self.wake.wait(0.5)
                self.wake.clear()
                continue
            self.current = job_id
            try:
                self._run_job(job_id)
            finally:
                self.current = None

    def _run_job(self, job_id: str) -> None:
        with self.lock:
            job = self.jobs.get(job_id)
        if job is None:  # deleted while queued
            return
        job["status"] = "running"
        job["progress"] = 0.05
        before = snapshot(self.download_dir)
        last = [""]

        def emit(line: str) -> None:
            line = line.strip()
            if not line:
                return
            last[0] = line[-200:]
            job["message"] = last[0]
            p = progress_from_line(line)
            if p is not None:
                job["progress"] = max(job["progress"], p)

        try:
            code = self.runner({k: job[k] for k in ("id", "url", "format", "lrc")}, self.download_dir, emit)
            after = snapshot(self.download_dir)
            new_files = sorted(after - before)
            if code != 0 and not new_files:
                job["status"] = "failed"
                job["message"] = last[0] or "spotdl exited with %s" % code
            else:
                job["status"] = "done"
                job["progress"] = 1.0
                job["files"] = new_files
                job["message"] = "%d file(s)" % len(new_files) if new_files else "nothing new (already downloaded?)"
        except Exception as e:  # noqa: BLE001
            job["status"] = "failed"
            job["message"] = str(e) or e.__class__.__name__
        log("job", job_id, job["status"], job["message"])

    # -- files

    def resolve_file(self, name: str) -> Optional[str]:
        """Absolute path of a servable file by bare name, or None (never leaves the download dir)."""
        if not name or name in (".", "..") or "/" in name or "\\" in name or "\x00" in name:
            return None
        if os.path.splitext(name)[1].lower() not in SERVE_EXT:
            return None
        full = os.path.realpath(os.path.join(self.download_dir, name))
        root = os.path.realpath(self.download_dir)
        if os.path.dirname(full) != root or not os.path.isfile(full):
            return None
        return full


class Handler(BaseHTTPRequestHandler):
    server: CompanionServer  # type: ignore[assignment]
    protocol_version = "HTTP/1.1"

    def _json(self, code: int, obj) -> None:
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _path(self) -> str:
        return unquote(urlsplit(self.path).path)

    def _status_page(self) -> None:
        port = self.server.server_address[1]
        addrs = ", ".join("<b>http://%s:%d</b>" % (ip, port) for ip in lan_ips()) or "<i>(no LAN address found)</i>"
        body = (
            "<html><body style='font-family:sans-serif;background:#0e0f13;color:#eee;padding:24px'>"
            "<h2>CUEd companion</h2>"
            "<p>Running. In CUEd → Downloads → Companion (LAN), enter one of: %s</p>"
            "<p>Jobs: %d · downloads folder: %s</p></body></html>"
        ) % (addrs, len(self.server.jobs), self.server.download_dir)
        b = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self) -> None:  # noqa: N802
        path = self._path()
        if path == "/health":
            self._json(200, "ok")
        elif path in ("", "/"):
            self._status_page()
        elif path == "/jobs":
            self._json(200, {"jobs": self.server.list_jobs()})
        elif path.startswith("/jobs/"):
            job = self.server.get_job(path[len("/jobs/"):])
            if job is None:
                return self._json(404, {"error": "no such job"})
            self._json(200, job)
        elif path.startswith("/files/"):
            full = self.server.resolve_file(path[len("/files/"):])
            if full is None:
                return self._json(404, {"error": "no such file"})
            mime = MIME.get(os.path.splitext(full)[1].lower(), "audio/mpeg")
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(os.path.getsize(full)))
            self.end_headers()
            with open(full, "rb") as f:
                while chunk := f.read(1 << 16):
                    self.wfile.write(chunk)
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self) -> None:  # noqa: N802
        if self._path() != "/jobs":
            return self._json(404, {"error": "not found"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            return self._json(400, {"error": "bad length"})
        if length > 1 << 20:
            return self._json(413, {"error": "too large"})
        raw = self.rfile.read(length) if length else b""
        try:
            req = json.loads(raw or b"{}")
        except (json.JSONDecodeError, UnicodeDecodeError):
            return self._json(400, {"error": "bad json"})
        if not isinstance(req, dict):
            return self._json(400, {"error": "bad json"})
        url = str(req.get("url", "")).strip()
        if not url or url.startswith("-"):
            return self._json(400, {"error": "missing url"})
        fmt = str(req.get("format", "mp3"))
        if fmt not in FORMATS:
            fmt = "mp3"
        job_id = self.server.submit(url, fmt, bool(req.get("lrc", False)))
        self._json(200, {"id": job_id})

    def do_DELETE(self) -> None:  # noqa: N802
        path = self._path()
        if not path.startswith("/jobs/"):
            return self._json(404, {"error": "not found"})
        job_id = path[len("/jobs/"):]
        code = self.server.delete_job(job_id)
        if code == 200:
            self._json(200, {"ok": True})
        elif code == 409:
            self._json(409, {"error": "job is not finished"})
        else:
            self._json(404, {"error": "no such job"})

    def log_message(self, fmt, *args):  # quieter default logging
        pass


def create_server(bind: str = "0.0.0.0", port: int = DEFAULT_PORT, download_dir: str = "downloads",
                  runner: Optional[Runner] = None) -> CompanionServer:
    """Bind the companion on ``bind:port`` (port 0 picks a free one) with its worker thread running.

    The caller drives it: ``srv.serve_forever()`` (often in a thread), then
    ``srv.shutdown(); srv.server_close()``.
    """
    return CompanionServer((bind, port), download_dir, runner)


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(prog="cued-companion", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--bind", default="0.0.0.0")
    ap.add_argument("--dir", default="downloads")
    ap.add_argument("--no-ffmpeg-check", action="store_true", help="do not look for / download ffmpeg at start-up")
    args = ap.parse_args(argv)
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")  # Windows consoles default to a legacy code page
    except Exception:  # noqa: BLE001
        pass
    try:
        import spotdl  # noqa: F401
    except ImportError:
        log("spotdl is not installed in this Python. Run:  python -m pip install spotdl")
        return 1
    if not args.no_ffmpeg_check:
        ensure_ffmpeg()
    try:
        srv = create_server(args.bind, args.port, args.dir)
    except OSError as e:
        log("cannot listen on %s:%d: %s" % (args.bind, args.port, e))
        return 1
    port = srv.server_address[1]
    log("CUEd companion listening on %s:%d  (downloads -> %s)" % (args.bind, port, srv.download_dir))
    if args.bind.startswith("127.") or args.bind == "localhost":
        log("  bound to loopback only (the desktop daemon proxies the phone's requests here)")
    else:
        for ip in lan_ips():
            log("  enter this in CUEd:  http://%s:%d" % (ip, port))
        log("  (Windows: allow it through the firewall if the phone can't reach it)")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        srv.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())

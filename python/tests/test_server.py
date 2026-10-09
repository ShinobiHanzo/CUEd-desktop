import json
import os
import threading
import time
import urllib.error
import urllib.request

import pytest

from cued_companion import server as srv_mod


def http(method, url, body=None, headers=None):
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=5) as r:
            return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def fake_runner(job, download_dir, emit):
    """Pretends to be spotdl: prints progress, writes a file named after the url."""
    emit("Processing query: " + job["url"])
    emit("Downloading 42%")
    name = job["url"].rsplit("/", 1)[-1] + "." + job["format"]
    with open(os.path.join(download_dir, name), "wb") as f:
        f.write(b"ID3fakeaudio")
    if job.get("lrc"):
        with open(os.path.join(download_dir, name.rsplit(".", 1)[0] + ".lrc"), "w") as f:
            f.write("[00:01.00] la la\n")
    emit("Downloaded 100%")
    return 0


def failing_runner(job, download_dir, emit):
    emit("LookupError: No results found")
    return 1


@pytest.fixture
def companion(tmp_path):
    def start(runner):
        s = srv_mod.create_server("127.0.0.1", 0, str(tmp_path / "dl"), runner=runner)
        t = threading.Thread(target=s.serve_forever, daemon=True)
        t.start()
        started.append(s)
        return s, "http://127.0.0.1:%d" % s.server_address[1]

    started = []
    yield start
    for s in started:
        s.shutdown()
        s.server_close()


def wait_done(base, job_id, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        st, _, body = http("GET", "%s/jobs/%s" % (base, job_id))
        assert st == 200
        job = json.loads(body)
        if job["status"] in ("done", "failed"):
            return job
        time.sleep(0.05)
    raise AssertionError("job did not finish")


def test_full_job_lifecycle(companion, tmp_path):
    s, base = companion(fake_runner)
    st, _, body = http("GET", base + "/health")
    assert st == 200 and json.loads(body) == "ok"

    st, _, body = http("POST", base + "/jobs", json.dumps({"url": "https://open.spotify.com/track/abc", "format": "mp3", "lrc": True}).encode(),
                       {"Content-Type": "application/json"})
    assert st == 200
    job_id = json.loads(body)["id"]
    assert len(job_id) == 12

    job = wait_done(base, job_id)
    assert job["status"] == "done"
    assert job["progress"] == 1.0
    assert job["files"] == ["abc.lrc", "abc.mp3"]
    assert job["message"] == "2 file(s)"
    assert set(job) >= {"status", "progress", "message", "files"}

    st, hdrs, body = http("GET", base + "/files/abc.mp3")
    assert st == 200 and body == b"ID3fakeaudio" and hdrs["Content-Type"] == "audio/mpeg"
    st, hdrs, body = http("GET", base + "/files/abc.lrc")
    assert st == 200 and body.startswith(b"[00:01.00]") and hdrs["Content-Type"].startswith("text/plain")

    st, _, body = http("GET", base + "/jobs")
    jobs = json.loads(body)["jobs"]
    assert st == 200 and [j["id"] for j in jobs] == [job_id] and jobs[0]["url"] == "https://open.spotify.com/track/abc"

    st, _, _ = http("DELETE", base + "/jobs/" + job_id)
    assert st == 200
    st, _, _ = http("GET", base + "/jobs/" + job_id)
    assert st == 404
    st, _, _ = http("DELETE", base + "/jobs/" + job_id)
    assert st == 404
    assert json.loads(http("GET", base + "/jobs")[2]) == {"jobs": []}


def test_progress_is_reported_while_running(companion):
    gate = threading.Event()

    def slow_runner(job, download_dir, emit):
        emit("Downloading 50%")
        gate.wait(5)
        with open(os.path.join(download_dir, "x.mp3"), "wb") as f:
            f.write(b"x")
        return 0

    s, base = companion(slow_runner)
    job_id = json.loads(http("POST", base + "/jobs", json.dumps({"url": "x"}).encode())[2])["id"]
    deadline = time.time() + 5
    job = None
    while time.time() < deadline:
        job = json.loads(http("GET", base + "/jobs/" + job_id)[2])
        if job["status"] == "running" and job["progress"] > 0.05:
            break
        time.sleep(0.02)
    assert job["status"] == "running"
    assert abs(job["progress"] - (0.05 + 0.85 * 0.5)) < 1e-9
    assert job["message"] == "Downloading 50%"
    st, _, _ = http("DELETE", base + "/jobs/" + job_id)
    assert st == 409  # not finished yet
    gate.set()
    assert wait_done(base, job_id)["status"] == "done"


def test_failed_job(companion):
    s, base = companion(failing_runner)
    job_id = json.loads(http("POST", base + "/jobs", json.dumps({"url": "nothing here"}).encode())[2])["id"]
    job = wait_done(base, job_id)
    assert job["status"] == "failed"
    assert job["message"] == "LookupError: No results found"
    assert job["files"] == []
    st, _, _ = http("DELETE", base + "/jobs/" + job_id)
    assert st == 200


def test_runner_exception_marks_failed(companion):
    def boom(job, download_dir, emit):
        raise RuntimeError("spotdl missing")

    s, base = companion(boom)
    job_id = json.loads(http("POST", base + "/jobs", json.dumps({"url": "x"}).encode())[2])["id"]
    job = wait_done(base, job_id)
    assert job["status"] == "failed" and job["message"] == "spotdl missing"


def test_bad_requests(companion):
    s, base = companion(fake_runner)
    st, _, body = http("POST", base + "/jobs", b"{not json", {"Content-Type": "application/json"})
    assert st == 400 and json.loads(body)["error"] == "bad json"
    st, _, body = http("POST", base + "/jobs", json.dumps({"url": "-x"}).encode())
    assert st == 400 and json.loads(body)["error"] == "missing url"
    st, _, body = http("POST", base + "/jobs", json.dumps({"url": "--download-ffmpeg"}).encode())
    assert st == 400
    st, _, _ = http("POST", base + "/jobs", json.dumps({}).encode())
    assert st == 400
    st, _, _ = http("POST", base + "/jobs", json.dumps([1, 2]).encode())
    assert st == 400
    st, _, _ = http("POST", base + "/other", b"{}")
    assert st == 404
    st, _, _ = http("GET", base + "/jobs/nope")
    assert st == 404
    st, _, _ = http("GET", base + "/nope")
    assert st == 404
    st, _, _ = http("DELETE", base + "/health")
    assert st == 404
    assert len(s.jobs) == 0


def test_unknown_format_falls_back_to_mp3(companion):
    s, base = companion(fake_runner)
    job_id = json.loads(http("POST", base + "/jobs", json.dumps({"url": "q/song", "format": "exe"}).encode())[2])["id"]
    job = wait_done(base, job_id)
    assert job["files"] == ["song.mp3"] and job["format"] == "mp3"


def test_files_never_leave_download_dir(companion, tmp_path):
    s, base = companion(fake_runner)
    secret = tmp_path / "secret.mp3"
    secret.write_bytes(b"outside")
    (tmp_path / "dl" / "inside.mp3").write_bytes(b"inside")
    (tmp_path / "dl" / "notes.txt").write_bytes(b"text")
    for path in ("/files/../etc/passwd", "/files/..%2Fetc%2Fpasswd", "/files/../secret.mp3", "/files/..%2Fsecret.mp3",
                 "/files/%2E%2E/secret.mp3", "/files/notes.txt", "/files/", "/files/.", "/files/..", "/files/missing.mp3"):
        st, _, body = http("GET", base + path)
        assert st == 404, path
        assert body != b"outside"
    # a symlink out of the folder is refused too
    try:
        os.symlink(str(secret), str(tmp_path / "dl" / "link.mp3"))
    except (OSError, NotImplementedError):
        pass
    else:
        assert http("GET", base + "/files/link.mp3")[0] == 404
    st, _, body = http("GET", base + "/files/inside.mp3")
    assert st == 200 and body == b"inside"
    # the phone percent-encodes names with spaces
    (tmp_path / "dl" / "A B - C.m4a").write_bytes(b"m4a")
    st, hdrs, body = http("GET", base + "/files/A%20B%20-%20C.m4a")
    assert st == 200 and body == b"m4a" and hdrs["Content-Type"] == "audio/mp4"


def test_status_page(companion):
    s, base = companion(fake_runner)
    st, hdrs, body = http("GET", base + "/")
    assert st == 200 and hdrs["Content-Type"].startswith("text/html")
    assert b"CUEd companion" in body and b"Jobs: 0" in body


def test_build_command_and_progress_parsing():
    cmd = srv_mod.build_command({"url": "https://x/y", "format": "opus", "lrc": True}, "/tmp/dl")
    assert cmd[1:4] == ["-m", "spotdl", "download"] and cmd[4] == "https://x/y"
    assert "--generate-lrc" in cmd and cmd[cmd.index("--format") + 1] == "opus"
    assert "--output" in cmd and cmd[cmd.index("--output") + 1].startswith("/tmp/dl")
    assert srv_mod.progress_from_line("Downloading 42%") == pytest.approx(0.05 + 0.85 * 0.42)
    assert srv_mod.progress_from_line("no numbers here") is None
    assert srv_mod.progress_from_line("500 items") is None
    assert isinstance(srv_mod.lan_ips(), list)


def test_spotdl_runner_uses_subprocess(monkeypatch, tmp_path):
    """The real runner only ever talks to subprocess; here subprocess is faked so nothing runs."""
    calls = []

    class FakeProc:
        def __init__(self, cmd, **kw):
            calls.append(cmd)
            import io
            self.stdout = io.StringIO("Downloading 10%\nDownloading 100%\n")

        def wait(self):
            return 0

    monkeypatch.setattr(srv_mod.subprocess, "Popen", FakeProc)
    lines = []
    code = srv_mod.spotdl_runner({"url": "https://x/y", "format": "mp3", "lrc": False}, str(tmp_path), lines.append)
    assert code == 0 and lines == ["Downloading 10%\n", "Downloading 100%\n"]
    assert calls and calls[0][2] == "spotdl"


def test_ensure_ffmpeg_asks_spotdl_when_missing(monkeypatch):
    monkeypatch.setattr(srv_mod.shutil, "which", lambda name: None)
    runs = []

    class R:
        returncode = 0
        stderr = b""

    def fake_run(cmd, **kw):
        runs.append(cmd)
        return R()

    monkeypatch.setattr(srv_mod.subprocess, "run", fake_run)
    assert srv_mod.ensure_ffmpeg() is True
    assert runs and runs[0][-1] == "--download-ffmpeg"
    monkeypatch.setattr(srv_mod.shutil, "which", lambda name: "/usr/bin/ffmpeg")
    runs.clear()
    assert srv_mod.ensure_ffmpeg() is True and runs == []


def test_main_refuses_without_spotdl(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def fake_import(name, *a, **kw):
        if name == "spotdl":
            raise ImportError("no spotdl")
        return real_import(name, *a, **kw)

    monkeypatch.setattr(builtins, "__import__", fake_import)
    assert srv_mod.main(["--port", "0", "--bind", "127.0.0.1", "--no-ffmpeg-check"]) == 1

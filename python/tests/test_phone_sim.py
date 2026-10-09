import hashlib
import json
import os
import struct
import sys
import wave

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fake_desktop import FakeDesktop  # noqa: E402

from cued_companion import links, nostr, phone_sim  # noqa: E402


def write_wav(path, seconds=0.25, rate=8000):
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        n = int(seconds * rate)
        w.writeframes(struct.pack("<%dh" % n, *[(i * 37) % 2000 - 1000 for i in range(n)]))
    return str(path)


@pytest.fixture
def desktop():
    d = FakeDesktop().start()
    yield d
    d.stop()


@pytest.fixture
def state_file(tmp_path):
    return str(tmp_path / "state.json")


def run(argv):
    return phone_sim.main(argv)


# ---------------------------------------------------------------- units


def test_pair_request_proof():
    link = links.PairLink(secret="0f" * 16, name="L", addresses=["http://x"])
    req = links.build_pair_request(link, "Sim Phone", "0011223344556677", "", nonce="aa" * 8)
    import hmac

    expected = hmac.new(bytes.fromhex("0f" * 16), b"0011223344556677|Sim Phone|" + b"aa" * 8, hashlib.sha256).hexdigest()
    assert req == {"v": 1, "name": "Sim Phone", "device": "0011223344556677", "pubkey": "", "nonce": "aa" * 8, "proof": expected}


def test_manifest_scanning(tmp_path):
    music = tmp_path / "music"
    (music / "sub").mkdir(parents=True)
    a = write_wav(music / "Evanescence - Afterlife.wav", seconds=0.5)
    b = write_wav(music / "sub" / "untitled.wav", seconds=0.125)
    (music / "readme.txt").write_text("not audio")
    entries = phone_sim.scan_dir(str(music), use_ffprobe=False)
    assert [e["key"] for e in entries] == ["Evanescence - Afterlife.wav", "sub/untitled.wav"]
    e = entries[0]
    assert e["artist"] == "Evanescence" and e["title"] == "Afterlife" and e["album"] == ""
    assert e["durationMs"] == 500 and e["mime"] == "audio/wav"
    assert e["size"] == os.path.getsize(a)
    assert e["sha256"] == hashlib.sha256(open(a, "rb").read()).hexdigest()
    assert e["genres"] == [] and e["bpm"] is None and e["link"] is None
    assert isinstance(e["modified"], int) and e["modified"] > 1_600_000_000_000
    assert entries[1]["artist"] == "" and entries[1]["title"] == "untitled" and entries[1]["durationMs"] == 125
    assert os.path.getsize(b) > 0
    with pytest.raises(phone_sim.SimError):
        phone_sim.scan_dir(str(tmp_path / "nope"))


def test_manifest_scanning_with_fake_ffprobe(tmp_path, monkeypatch):
    music = tmp_path / "m"
    music.mkdir()
    write_wav(music / "x.wav")
    calls = []

    class R:
        stdout = json.dumps({"format": {"duration": "3.5", "tags": {"TITLE": "Real Title", "artist": "Real Artist", "album": "LP"}}}).encode()

    def fake_run(cmd, **kw):
        calls.append(cmd)
        return R()

    monkeypatch.setattr(phone_sim.subprocess, "run", fake_run)
    monkeypatch.setattr(phone_sim.shutil, "which", lambda n: "/usr/bin/ffprobe")
    e = phone_sim.scan_dir(str(music))[0]
    assert calls and calls[0][0] == "/usr/bin/ffprobe" and calls[0][-1].endswith("x.wav")
    assert (e["title"], e["artist"], e["album"], e["durationMs"]) == ("Real Title", "Real Artist", "LP", 3500)
    # ffprobe absent -> filename fallback, no subprocess
    calls.clear()
    monkeypatch.setattr(phone_sim.shutil, "which", lambda n: None)
    e = phone_sim.scan_dir(str(music))[0]
    assert calls == [] and e["title"] == "x"


def test_state_round_trip_and_identity(state_file):
    st = phone_sim.load_state(state_file)
    assert st == {}
    dev = phone_sim.ensure_device(st)
    sk, pk = phone_sim.ensure_identity(st)
    assert len(dev) == 16 and nostr.public_key(bytes.fromhex(sk)).hex() == pk
    phone_sim.save_state(state_file, st)
    again = phone_sim.load_state(state_file)
    assert again["device"] == dev and again["identity"]["pubkey"] == pk
    assert phone_sim.ensure_identity(again) == (sk, pk)
    with pytest.raises(phone_sim.SimError):
        phone_sim.require_paired(again)


def test_safe_filename_and_blob_name():
    assert phone_sim.safe_filename('a/b:c*"d') == "a_b_c__d"
    assert phone_sim.safe_filename("...") == "track"
    assert phone_sim.BLOB_NAME.match("playlists.v2_export-1.json")
    assert not phone_sim.BLOB_NAME.match("../etc")


# ---------------------------------------------------------------- against the fake desktop


def test_pair_sync_status_cli(desktop, state_file, tmp_path, capsys, monkeypatch):
    monkeypatch.setattr(phone_sim, "CHUNK_SIZE", 1000)  # several chunks per file
    music = tmp_path / "music"
    music.mkdir()
    write_wav(music / "Evanescence - Afterlife.wav", seconds=0.5)   # 8000 bytes + header -> 9 chunks
    write_wav(music / "Nirvana - Breed.wav", seconds=0.05)          # one chunk

    link = desktop.new_pair_link()
    assert run(["--state", state_file, "pair", link.encode(), "--name", "Sim Phone"]) == 0
    out = capsys.readouterr().out
    assert "paired with Fake desktop" in out
    state = json.load(open(state_file))
    assert state["desktop"] == desktop.base and state["token"] in desktop.tokens
    assert desktop.devices[state["device"]]["pubkey"] == state["identity"]["pubkey"]
    assert link.secret not in desktop.secrets  # one secret, one phone

    # the same QR again is refused (secret spent) -> 403 -> exit 1
    assert run(["--state", state_file, "pair", link.encode()]) == 1
    assert "wrong or expired secret" in capsys.readouterr().err
    # a fresh secret but the same device id -> 409
    assert run(["--state", state_file, "pair", desktop.new_pair_link().encode()]) == 1
    assert "already paired" in capsys.readouterr().err

    assert run(["--state", state_file, "sync", str(music), "--no-ffprobe"]) == 0
    out = capsys.readouterr().out
    assert "scanned 2 track(s)" in out and "2 to upload" in out and "done: 2 uploaded, 0 already there, 0 failed" in out
    assert len(desktop.complete) == 2
    puts = [p for m, p in desktop.requests if m == "PUT"]
    assert len(puts) == 9 + 1
    heads = [p for m, p in desktop.requests if m == "HEAD"]
    assert len(heads) == 2
    for sha, entry in desktop.complete.items():
        assert hashlib.sha256(entry["bytes"]).hexdigest() == sha
        assert entry["meta"]["sha256"] == sha and entry["meta"]["artist"] in ("Evanescence", "Nirvana")

    # second sync: nothing missing, nothing uploaded
    desktop.requests.clear()
    assert run(["--state", state_file, "sync", str(music), "--no-ffprobe"]) == 0
    assert "0 to upload" in capsys.readouterr().out
    assert not [p for m, p in desktop.requests if m in ("PUT", "HEAD")]

    assert run(["--state", state_file, "status"]) == 0
    status = json.loads(capsys.readouterr().out)
    dev = status["devices"][state["device"]]
    assert dev["tracksOnPhone"] == 2 and dev["tracksBackedUp"] == 2 and dev["lastUpload"]

    assert run(["--state", state_file, "snapshot"]) == 0
    assert json.loads(capsys.readouterr().out)["tracks"] == 2
    assert len(desktop.snapshots[state["device"]]) == 1


def test_sync_resumes_partial_upload(desktop, state_file, tmp_path, monkeypatch):
    monkeypatch.setattr(phone_sim, "CHUNK_SIZE", 1024)
    music = tmp_path / "music"
    music.mkdir()
    path = write_wav(music / "A - B.wav", seconds=0.5)
    data = open(path, "rb").read()
    sha = hashlib.sha256(data).hexdigest()
    link = desktop.new_pair_link()
    assert run(["--state", state_file, "pair", link.encode()]) == 0
    # the desktop already holds the first 3000 bytes from an interrupted upload
    desktop.partial[sha] = {"bytes": bytearray(data[:3000]), "meta": None, "total": len(data)}
    token = json.load(open(state_file))["token"]
    log = []
    summary = phone_sim.sync_dir(desktop.base, token, str(music), 1024, use_ffprobe=False, log=log.append)
    assert summary["uploaded"] == 1 and summary["bytes"] == len(data)
    assert any("resuming A - B.wav at 3000" in l for l in log)
    puts = [p for m, p in desktop.requests if m == "PUT"]
    assert len(puts) == -(-(len(data) - 3000) // 1024)
    assert desktop.complete[sha]["bytes"] == data
    assert desktop.complete[sha]["meta"]["title"] == "B"


def test_sync_reports_hash_mismatch(desktop, state_file, tmp_path, monkeypatch):
    music = tmp_path / "music"
    music.mkdir()
    path = write_wav(music / "x.wav")
    link = desktop.new_pair_link()
    assert run(["--state", state_file, "pair", link.encode()]) == 0
    token = json.load(open(state_file))["token"]
    entries = phone_sim.scan_dir(str(music), use_ffprobe=False)
    e = dict(entries[0], sha256="00" * 32)  # lie about the hash
    with pytest.raises(phone_sim.SimError, match="did not match"):
        phone_sim.upload_track(desktop.base, token, e, path)
    assert "00" * 32 not in desktop.complete and "00" * 32 not in desktop.partial


def test_unpaired_commands_fail_cleanly(state_file, tmp_path, capsys):
    assert run(["--state", state_file, "status"]) == 1
    assert "not paired" in capsys.readouterr().err
    assert run(["--state", state_file, "pair", "cued://share?t=x"]) == 1
    assert "not a cued://pair" in capsys.readouterr().err
    dead = links.PairLink(secret="aa" * 16, addresses=["http://127.0.0.1:9"])  # nothing listens on 9
    assert run(["--state", state_file, "pair", dead.encode()]) == 2
    assert "no address answered" in capsys.readouterr().err


def test_blob_put_get(desktop, state_file, tmp_path, capsys):
    assert run(["--state", state_file, "pair", desktop.new_pair_link().encode()]) == 0
    src = tmp_path / "playlists.json"
    src.write_bytes(b'{"lists": [1, 2, 3]}')
    assert run(["--state", state_file, "blob", "put", "playlists.json", str(src)]) == 0
    assert run(["--state", state_file, "blob", "get", "playlists.json", str(tmp_path / "back.json")]) == 0
    assert (tmp_path / "back.json").read_bytes() == src.read_bytes()
    assert run(["--state", state_file, "blob", "get", "missing.bin", str(tmp_path / "x")]) == 1
    assert run(["--state", state_file, "blob", "put", "../bad", str(src)]) == 1
    capsys.readouterr()


def test_receive_share(desktop, tmp_path, capsys):
    data = open(write_wav(tmp_path / "src.wav"), "rb").read()
    sha = hashlib.sha256(data).hexdigest()
    desktop.shares[(sha, "cap123")] = data
    payload = links.SharePayload(title="Afterlife (Live)", artist="Evanescence", file_url="%s/share/%s/cap123" % (desktop.base, sha), genres=["rock"], bpm=128.0)
    out_dir = tmp_path / "received"
    assert run(["receive", payload.encode(), "--dir", str(out_dir)]) == 0
    dest = out_dir / "Evanescence - Afterlife (Live).wav"
    assert dest.read_bytes() == data
    assert "received" in capsys.readouterr().out
    # no f -> the phone would queue the link instead; we say so and fail
    assert run(["receive", links.SharePayload(title="x", link="https://open.spotify.com/track/a").encode(), "--dir", str(out_dir)]) == 1
    assert "no file url" in capsys.readouterr().err


def test_station_without_websockets_is_a_clear_error(state_file, monkeypatch, capsys):
    import builtins

    real_import = builtins.__import__

    def fake_import(name, *a, **kw):
        if name == "websockets":
            raise ImportError("nope")
        return real_import(name, *a, **kw)

    monkeypatch.setattr(builtins, "__import__", fake_import)
    assert run(["--state", state_file, "station", "--relay", "ws://127.0.0.1:9/relay", "--name", "S", "--title", "T", "--artist", "A", "--seconds", "1"]) == 3
    assert "websockets" in capsys.readouterr().err


def test_station_publishes_signed_events(state_file, capsys):
    """Runs a tiny NIP-01 relay in-process when websockets is installed."""
    websockets = pytest.importorskip("websockets")
    import asyncio
    import threading

    received = []
    ready = threading.Event()
    port_holder = []

    async def relay_main():
        async def handler(ws):
            async for msg in ws:
                verb, rest = nostr.parse_message(msg)
                if verb == "EVENT":
                    ev = rest[0]
                    ok = nostr.verify_event(ev)
                    received.append((ev, ok))
                    await ws.send(json.dumps(["OK", ev["id"], ok, "" if ok else "invalid: bad signature"]))

        async with websockets.serve(handler, "127.0.0.1", 0) as server:
            port_holder.append(server.sockets[0].getsockname()[1])
            ready.set()
            await stop.wait()

    loop = asyncio.new_event_loop()
    stop = asyncio.Event()

    def run_loop():
        asyncio.set_event_loop(loop)
        loop.run_until_complete(relay_main())

    t = threading.Thread(target=run_loop, daemon=True)
    t.start()
    assert ready.wait(5)
    try:
        code = run(["--state", state_file, "station", "--relay", "ws://127.0.0.1:%d/relay" % port_holder[0], "--name", "Sim", "--title", "Afterlife",
                    "--artist", "Evanescence", "--seconds", "0.3", "--interval", "0.1"])
        out = capsys.readouterr().out
        assert code == 0, out
        assert "follow link: cued://station?p=" in out
        assert len(received) >= 3 and all(ok for _, ok in received)
        first = received[0][0]
        assert first["kind"] == 30777 and first["tags"] == [["d", "cued"], ["t", "cued-station"]]
        content = json.loads(first["content"])
        assert content["status"] == "on_air" and content["now"]["title"] == "Afterlife" and content["seq"] == 1
        assert json.loads(received[-1][0]["content"])["status"] == "off"
        state = json.load(open(state_file))
        assert first["pubkey"] == state["identity"]["pubkey"]
    finally:
        loop.call_soon_threadsafe(stop.set)
        t.join(5)

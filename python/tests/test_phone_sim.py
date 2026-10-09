import hashlib
import json
import os
import struct
import sys
import time
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


# ---------------------------------------------------------------- round 2: accounts, assertions, friends, listening (§7–§11)

from cued_companion import account as account_mod  # noqa: E402
from cued_companion import bio, seal  # noqa: E402


def paired_state(desktop, state_file, mode="separate", name="Sim Phone"):
    assert run(["--state", state_file, "pair", desktop.new_pair_link().encode(), "--name", name, "--account", mode]) == 0
    return json.load(open(state_file))


def test_pair_registers_biokey_and_devhash_separate(desktop, state_file, capsys):
    state = paired_state(desktop, state_file)
    out = capsys.readouterr().out
    assert "biokey" in out and "separate accounts" in out
    dev = desktop.devices[state["device"]]
    assert dev["account"] == "separate"
    assert dev["biokey"] == state["biokey"]["pub"] and bio.valid_biokey(dev["biokey"])
    assert dev["devhash"] == account_mod.devhash(state["device"], state["devsalt"]) and len(state["devsalt"]) == 32
    assert "chain" not in state and desktop.chain is None  # nothing shared
    # the biometric key and salt are stable across runs
    st2 = phone_sim.load_state(state_file)
    assert phone_sim.ensure_biokey(st2) == (state["biokey"]["priv"], state["biokey"]["pub"])
    assert phone_sim.ensure_devhash(st2) == dev["devhash"]


def test_pair_join_takes_the_desktops_account(desktop, state_file, capsys):
    assert desktop.chain is None
    old_identity = phone_sim.ensure_identity(phone_sim.load_state(state_file))
    state = paired_state(desktop, state_file, "join", name="Pixel 8")
    out = capsys.readouterr().out
    assert "joined account" in out
    # the desktop made an account on the spot and bound the phone's devhash before sealing
    assert desktop.chain is not None and desktop.chain.name() == desktop.name
    assert state["identity"]["secret"] == desktop.secret and state["identity"]["pubkey"] == desktop.pubkey
    assert state["identity"]["secret"] != old_identity[0]
    chain = account_mod.Chain.from_dict(state["chain"])
    chain.validate()
    assert chain.pubkey == desktop.pubkey and chain.to_dict() == desktop.chain.to_dict()
    devs = chain.devices()
    assert {d["kind"] for d in devs} == {"desktop", "phone"}
    assert any(d["devhash"] == desktop.devices[state["device"]]["devhash"] and d["name"] == "Pixel 8" for d in devs)
    # account show / chain agree, and the sim's copy is already current
    assert run(["--state", state_file, "account", "show"]) == 0
    shown = json.loads(capsys.readouterr().out)
    assert shown["pubkey"] == desktop.pubkey and shown["height"] == chain.height and shown["id"] == account_mod.account_id(desktop.pubkey)
    assert run(["--state", state_file, "account", "chain"]) == 0
    out = capsys.readouterr().out
    assert "height %d" % chain.height in out and "already current" in out


def test_pair_join_without_sealed_bundle_is_an_error(desktop, state_file, capsys, monkeypatch):
    real = phone_sim.pair_with

    def strip_account(*a, **kw):
        base, resp, pin = real(*a, **kw)
        resp.pop("account", None)
        return base, resp, pin

    monkeypatch.setattr(phone_sim, "pair_with", strip_account)
    assert run(["--state", state_file, "pair", desktop.new_pair_link().encode(), "--account", "join"]) == 1
    assert "no account.sealed" in capsys.readouterr().err
    assert json.load(open(state_file)).get("token")  # the pairing itself stuck


def test_open_account_bundle_rejects_bad_bundles():
    secret = "0f" * 16
    sk = nostr.generate_secret().hex()
    chain = account_mod.Chain.create(sk, "n", 1)
    good = seal.seal(bytes.fromhex(secret), "account", json.dumps(phone_sim.account_bundle(sk, chain)).encode())
    assert phone_sim.open_account_bundle(secret, good)[1].to_dict() == chain.to_dict()
    with pytest.raises(phone_sim.SimError, match="wrong secret"):
        phone_sim.open_account_bundle("00" * 16, good)
    other = nostr.generate_secret().hex()
    mismatch = seal.seal(bytes.fromhex(secret), "account", json.dumps(phone_sim.account_bundle(other, chain)).encode())
    with pytest.raises(phone_sim.SimError, match="does not match"):
        phone_sim.open_account_bundle(secret, mismatch)
    junk = seal.seal(bytes.fromhex(secret), "account", b"not json")
    with pytest.raises(phone_sim.SimError, match="understands"):
        phone_sim.open_account_bundle(secret, junk)


def test_pair_keep_gives_the_desktop_the_phones_account(desktop, state_file, capsys):
    desktop.ensure_account()  # the desktop had its own account before
    before = desktop.secret
    state = paired_state(desktop, state_file, "keep", name="Pixel 8")
    out = capsys.readouterr().out
    assert "desktop now uses this phone's account" in out
    # the sim made a chain first: create + bind_device(phone); the desktop then appended its own bind
    chain = account_mod.Chain.from_dict(state["chain"])
    chain.validate()
    assert [b.type for b in chain.blocks] == ["create", "bind_device"]
    assert chain.name() == "Pixel 8" and chain.pubkey == state["identity"]["pubkey"]
    assert desktop.secret == state["identity"]["secret"] != before
    assert desktop.chain.pubkey == chain.pubkey and desktop.chain.height == 3
    assert {d["kind"] for d in desktop.chain.devices()} == {"phone", "desktop"}
    assert ("POST", "/api/account/import") in desktop.requests
    # the desktop now has the longer chain: account chain merges it into the sim's copy
    assert run(["--state", state_file, "account", "chain"]) == 0
    assert "merged into the sim's copy (now height 3)" in capsys.readouterr().out
    assert len(json.load(open(state_file))["chain"]["blocks"]) == 3
    # push: a longer local chain wins on the desktop
    st = phone_sim.load_state(state_file)
    c = phone_sim.load_chain(st)
    c.append(st["identity"]["secret"], "profile", {"name": "Idris"}, int(time.time()))
    phone_sim.store_chain(st, c)
    phone_sim.save_state(state_file, st)
    assert run(["--state", state_file, "account", "push"]) == 0
    assert "pushed 4 block(s)" in capsys.readouterr().out
    assert desktop.chain.height == 4 and desktop.chain.name() == "Idris"
    # pushing the same chain again changes nothing; a foreign chain is refused
    assert run(["--state", state_file, "account", "push"]) == 0
    assert '"changed": false' in capsys.readouterr().out
    foreign = account_mod.Chain.create(nostr.generate_secret().hex(), "x", 1)
    status, resp = phone_sim.request_json("POST", desktop.base + "/api/account/chain", foreign.to_dict(), st["token"])
    assert status == 409 and "different account" in resp["error"]


def test_account_push_without_chain_and_separate_desktop_chain(desktop, state_file, capsys):
    paired_state(desktop, state_file)
    assert run(["--state", state_file, "account", "push"]) == 1
    assert "no account chain" in capsys.readouterr().err
    # the desktop's own chain validates and is reported, not merged (different account)
    st = phone_sim.load_state(state_file)
    phone_sim.ensure_chain(st, "Sim Phone")
    phone_sim.save_state(state_file, st)
    assert run(["--state", state_file, "account", "chain"]) == 0
    out = capsys.readouterr().out
    assert "desktop chain" in out and "different account" in out
    # a desktop chain that does not validate is refused
    desktop.chain.blocks[0].data["name"] = "tampered"
    assert run(["--state", state_file, "account", "chain"]) == 1
    assert "does not validate" in capsys.readouterr().err


def test_settings_get_set(desktop, state_file, capsys):
    paired_state(desktop, state_file)
    capsys.readouterr()
    assert run(["--state", state_file, "settings", "get"]) == 0
    s = json.loads(capsys.readouterr().out)
    assert s["relayEnabled"] is False and s["theme"]["accent"] == "#5ef2c6"
    assert run(["--state", state_file, "settings", "set", '{"relayEnabled": true, "theme": {"accent": "#fff"}}']) == 0
    s = json.loads(capsys.readouterr().out)
    assert s["relayEnabled"] is True and s["theme"] == {"accent": "#fff"}
    assert desktop.chain.latest("settings") == {"relayEnabled": True, "theme": {"accent": "#fff"}}
    assert desktop.chain.is_valid()
    assert run(["--state", state_file, "settings", "set", "[1]"]) == 1
    assert "JSON object" in capsys.readouterr().err
    assert run(["--state", state_file, "settings", "set", "{nope"]) == 1
    capsys.readouterr()


def test_friends_flow(desktop, state_file, capsys):
    paired_state(desktop, state_file)
    capsys.readouterr()
    assert run(["--state", state_file, "friends", "link"]) == 0
    link = capsys.readouterr().out.strip()
    fl = links.FriendLink.decode(link)
    assert fl is not None and fl.pubkey == desktop.pubkey and fl.name == desktop.name
    other = nostr.generate_secret()
    other_pk = nostr.public_key(other).hex()
    assert run(["--state", state_file, "friends", "add", links.FriendLink(other_pk, "Ana", ["wss://nos.lol"]).encode()]) == 0
    assert json.loads(capsys.readouterr().out) == {"pubkey": other_pk, "name": "Ana", "status": "pending_out"}
    assert run(["--state", state_file, "friends", "requests"]) == 0
    assert json.loads(capsys.readouterr().out)["requests"][0]["status"] == "pending_out"
    assert run(["--state", state_file, "friends", "list"]) == 0
    assert json.loads(capsys.readouterr().out) == {"friends": []}
    # an incoming request, accepted by pubkey / npub
    third = nostr.public_key(nostr.generate_secret()).hex()
    desktop.friends[third] = {"pubkey": third, "name": "Bo", "status": "pending_in"}
    assert run(["--state", state_file, "friends", "accept", nostr.npub(third)]) == 0
    assert json.loads(capsys.readouterr().out)["status"] == "friends"
    assert run(["--state", state_file, "friends", "list"]) == 0
    assert [f["name"] for f in json.loads(capsys.readouterr().out)["friends"]] == ["Bo"]
    assert run(["--state", state_file, "friends", "remove", third]) == 0
    capsys.readouterr()
    assert third not in desktop.friends
    assert run(["--state", state_file, "friends", "remove", third]) == 1
    assert "HTTP 404" in capsys.readouterr().err
    assert run(["--state", state_file, "friends", "accept", "notakey"]) == 1
    assert "not a pubkey" in capsys.readouterr().err
    assert run(["--state", state_file, "friends", "add"]) == 1
    assert run(["--state", state_file, "friends", "add", "garbage"]) == 1
    capsys.readouterr()
    # adding by npub alone
    assert run(["--state", state_file, "friends", "add", nostr.npub(third)]) == 0
    assert json.loads(capsys.readouterr().out)["pubkey"] == third


def test_listen_start_status_stop(desktop, state_file, capsys):
    paired_state(desktop, state_file)
    capsys.readouterr()
    pk = nostr.public_key(nostr.generate_secret()).hex()
    desktop.friends[pk] = {"pubkey": pk, "name": "Ana", "status": "friends"}
    assert run(["--state", state_file, "listen", "status"]) == 0
    assert json.loads(capsys.readouterr().out) == {"listening": None}
    assert run(["--state", state_file, "listen", nostr.npub(pk)]) == 0
    out = capsys.readouterr().out
    assert out.startswith("listening to npub1") and '"name": "Ana"' in out
    assert desktop.listening == pk
    assert run(["--state", state_file, "listen", "status"]) == 0
    assert json.loads(capsys.readouterr().out)["listening"]["pubkey"] == pk
    assert run(["--state", state_file, "listen", "stop"]) == 0
    assert "stopped" in capsys.readouterr().out and desktop.listening is None
    assert run(["--state", state_file, "listen", "zz"]) == 1
    capsys.readouterr()


def test_lyrics_and_cover(desktop, state_file, tmp_path, capsys):
    paired_state(desktop, state_file)
    capsys.readouterr()
    sha = "ab" * 32
    desktop.lyrics[sha] = "[00:01.00] Hello\n[00:05.00] World"
    desktop.covers[sha] = ("image/jpeg", b"\xff\xd8\xff\xe0fakejpeg")
    assert run(["--state", state_file, "lyrics", sha]) == 0
    assert capsys.readouterr().out == "[00:01.00] Hello\n[00:05.00] World\n"
    out = tmp_path / "cover.jpg"
    assert run(["--state", state_file, "cover", sha, str(out)]) == 0
    assert out.read_bytes() == b"\xff\xd8\xff\xe0fakejpeg" and "image/jpeg" in capsys.readouterr().out
    assert run(["--state", state_file, "lyrics", "cd" * 32]) == 1
    assert "no lyrics" in capsys.readouterr().err
    assert run(["--state", state_file, "cover", "cd" * 32, str(out)]) == 1
    assert "no cover" in capsys.readouterr().err
    assert run(["--state", state_file, "lyrics", "nothex"]) == 1
    capsys.readouterr()


def test_receive_prints_friend_request(desktop, tmp_path, capsys):
    data = open(write_wav(tmp_path / "src.wav"), "rb").read()
    sha = hashlib.sha256(data).hexdigest()
    desktop.shares[(sha, "cap")] = data
    pk = "ab" * 32
    payload = links.SharePayload(title="Song", artist="Band", file_url="%s/share/%s/cap" % (desktop.base, sha), friend_pubkey=pk, friend_name="Idris")
    assert run(["receive", payload.encode(), "--dir", str(tmp_path / "r")]) == 0
    out = capsys.readouterr().out
    assert "friend request from Idris (%s)" % nostr.npub(pk) in out and "friends add " + pk in out
    plain = links.SharePayload(title="Song", artist="Band", file_url="%s/share/%s/cap" % (desktop.base, sha))
    assert run(["receive", plain.encode(), "--dir", str(tmp_path / "r")]) == 0
    assert "friend request" not in capsys.readouterr().out


def test_remote_mode_assertion_and_read_only(desktop, state_file, tmp_path, capsys, monkeypatch):
    """The fake has no TLS; pretend its http base is the funnel and let it enforce §7."""
    state = paired_state(desktop, state_file)
    sha = "ab" * 32
    desktop.lyrics[sha] = "la la"
    desktop.remote = True
    monkeypatch.setattr(phone_sim, "is_remote", lambda base: True)
    desktop.requests.clear()
    desktop.assertions.clear()
    # a read: challenge fetched (bearer only), assertion attached, accepted
    assert run(["--state", state_file, "status"]) == 0
    capsys.readouterr()
    assert [p for m, p in desktop.requests] == ["/api/auth/challenge", "/api/sync/status"]
    assert len(desktop.assertions) == 1 and desktop.assertions[0][0] == "/api/sync/status"
    header = desktop.assertions[0][1]
    assert bio.verify_assertion(state["biokey"]["pub"], state["device"], header)
    st = json.load(open(state_file))
    assert st["assertion"]["value"] == header and st["assertion"]["expires"] > time.time() + 8 * 60
    # cached: a second read reuses it, no new challenge
    desktop.requests.clear()
    assert run(["--state", state_file, "lyrics", sha]) == 0
    assert capsys.readouterr().out == "la la\n"
    assert [p for m, p in desktop.requests] == ["/api/track/%s/lyrics" % sha]
    assert run(["--state", state_file, "account", "show"]) == 0
    assert run(["--state", state_file, "settings", "get"]) == 0
    assert run(["--state", state_file, "friends", "list"]) == 0
    assert run(["--state", state_file, "listen", "status"]) == 0
    capsys.readouterr()
    # expired: refreshed before use
    st["assertion"]["expires"] = int(time.time()) - 1
    json.dump(st, open(state_file, "w"))
    desktop.requests.clear()
    assert run(["--state", state_file, "status"]) == 0
    capsys.readouterr()
    assert [p for m, p in desktop.requests] == ["/api/auth/challenge", "/api/sync/status"]
    assert json.load(open(state_file))["assertion"]["value"] != header
    # writes are refused locally with a clear message, before any request
    desktop.requests.clear()
    music = tmp_path / "music"
    music.mkdir()
    write_wav(music / "x.wav")
    assert run(["--state", state_file, "sync", str(music), "--no-ffprobe"]) == 1
    assert "uploads only on the LAN" in capsys.readouterr().err
    for argv in (["snapshot"], ["account", "push"], ["settings", "set", "{}"], ["friends", "add", nostr.npub(desktop.pubkey)],
                 ["friends", "accept", desktop.pubkey], ["friends", "remove", desktop.pubkey], ["listen", desktop.pubkey], ["listen", "stop"],
                 ["blob", "put", "a", str(music / "x.wav")]):
        assert run(["--state", state_file] + argv) == 1, argv
        assert "only on the LAN" in capsys.readouterr().err
    assert desktop.requests == []
    # and the desktop would refuse them anyway (403 before a handler runs)
    token = st["token"]
    phone_sim.EXTRA_HEADERS[phone_sim.ASSERTION_HEADER] = json.load(open(state_file))["assertion"]["value"]
    status, resp = phone_sim.request_json("POST", desktop.base + "/api/backup/snapshot", {}, token)
    assert status == 403 and resp["error"] == "writes only on the LAN"
    status, resp = phone_sim.request_json("GET", desktop.base + "/api/track/%s/meta" % sha, None, token)  # on the list (even if 404 here)
    assert status != 403
    # without an assertion, or with one for another device, reads are refused
    phone_sim.EXTRA_HEADERS.clear()
    status, resp = phone_sim.request_json("GET", desktop.base + "/api/sync/status", None, token)
    assert status == 403 and "assertion" in resp["error"]
    bad = bio.assertion(bio.generate()[0], state["device"], header.split(".")[0])
    phone_sim.EXTRA_HEADERS[phone_sim.ASSERTION_HEADER] = bad
    status, resp = phone_sim.request_json("GET", desktop.base + "/api/sync/status", None, token)
    assert status == 403 and resp["error"] == "bad assertion"
    phone_sim.EXTRA_HEADERS.clear()


def test_remote_pair_keep_refuses_import(desktop, state_file, capsys, monkeypatch):
    monkeypatch.setattr(phone_sim, "is_remote", lambda base: True)
    assert run(["--state", state_file, "pair", desktop.new_pair_link().encode(), "--account", "keep"]) == 1
    assert "account import only on the LAN" in capsys.readouterr().err
    st = json.load(open(state_file))
    assert st["token"] and st["chain"]  # paired, chain made locally, nothing imported
    assert ("POST", "/api/account/import") not in desktop.requests


def test_challenge_failure_is_clear(desktop, state_file, capsys, monkeypatch):
    paired_state(desktop, state_file)
    monkeypatch.setattr(phone_sim, "is_remote", lambda base: True)
    st = phone_sim.load_state(state_file)
    st["token"] = "nope"
    phone_sim.save_state(state_file, st)
    assert run(["--state", state_file, "status"]) == 1
    assert "challenge: HTTP 401" in capsys.readouterr().err

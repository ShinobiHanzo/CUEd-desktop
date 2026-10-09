"""
End-to-end against a real ``cuedd``. Skipped unless the environment names one:

    CUEDD_URL=http://127.0.0.1:8770 CUEDD_PAIR_LINK="cued://pair?..." python -m pytest tests/test_e2e.py

The pair link comes from the desktop UI's "Link a phone" page (or the daemon's
``/api/pair/link`` if it has one). The test pairs, syncs one generated WAV and
checks ``/api/library`` lists it.
"""
import hashlib
import json
import os
import struct
import wave

import pytest

from cued_companion import phone_sim

CUEDD_URL = os.environ.get("CUEDD_URL")
PAIR_LINK = os.environ.get("CUEDD_PAIR_LINK")

pytestmark = pytest.mark.skipif(not CUEDD_URL, reason="set CUEDD_URL (and CUEDD_PAIR_LINK) to run against a live cuedd")


def test_pair_sync_library(tmp_path):
    if not PAIR_LINK:
        pytest.skip("CUEDD_PAIR_LINK not set; the desktop's Link-a-phone page shows one")
    state = str(tmp_path / "state.json")
    assert phone_sim.main(["--state", state, "pair", PAIR_LINK, "--name", "pytest phone"]) == 0
    music = tmp_path / "music"
    music.mkdir()
    path = music / "Pytest - Sine.wav"
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(8000)
        w.writeframes(struct.pack("<4000h", *[(i * 91) % 4000 - 2000 for i in range(4000)]))
    sha = hashlib.sha256(path.read_bytes()).hexdigest()
    assert phone_sim.main(["--state", state, "sync", str(music)]) == 0
    st = json.load(open(state))
    status, lib = phone_sim.request_json("GET", st["desktop"] + "/api/library?q=Sine&limit=50", token=st["token"])
    assert status == 200, lib
    assert any(t.get("sha256") == sha for t in lib["tracks"]), lib
    assert phone_sim.main(["--state", state, "status"]) == 0

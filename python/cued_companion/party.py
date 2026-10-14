"""A party guest from the command line (`docs/protocol.md` §13): finds hosts on
the LAN with the UDP beacon, signs requests with a station key and talks to a
host's `/api/party` (desktop) or `/party` (phone).

    cued-party --nsec nsec1… discover
    cued-party --nsec nsec1… --url http://192.168.1.9:8770 hello
    cued-party --nsec nsec1… --url … search "daft punk"
    cued-party --nsec nsec1… --url … queue <sha256>
    cued-party --nsec nsec1… --url … now|skip|pause|play

The key must be in the host's friends list, or the host answers "not a friend".
Pure Python; nothing to install.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional

from . import nostr

BEACON_PORT = 8772
BEACON_QUERY = b"CUED-PARTY?"
BEACON_PREFIX = "CUED-PARTY "
MAX_SKEW_SEC = 90


def canonical(v: Any) -> str:
    """Keys sorted, no whitespace: the same bytes the phone and the desktop hash."""
    if isinstance(v, dict):
        return "{" + ",".join(json.dumps(k, ensure_ascii=False) + ":" + canonical(v[k]) for k in sorted(v)) + "}"
    if isinstance(v, list):
        return "[" + ",".join(canonical(x) for x in v) + "]"
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=False)
    if v is True:
        return "true"
    if v is False:
        return "false"
    if v is None:
        return "null"
    if isinstance(v, float) and v.is_integer():
        return str(int(v))
    return json.dumps(v)


def digest(env: Dict[str, Any]) -> bytes:
    body = "cued-party|%d|%s|%d|%s|%s|%d|%s" % (env.get("v", 1), env["pubkey"], env["ts"], env["nonce"], env["op"], 1 if env.get("ok", True) else 0, canonical(env.get("arg")))
    return nostr.tagged_hash("CUEd/party", body.encode("utf-8"))


def sign(sk: bytes, op: str, arg: Any = None, ok: bool = True, nonce: Optional[str] = None, ts: Optional[int] = None) -> Dict[str, Any]:
    env = {
        "v": 1,
        "pubkey": nostr.public_key(sk).hex(),
        "ts": int(time.time()) if ts is None else ts,
        "nonce": nonce or os.urandom(8).hex(),
        "op": op,
        "arg": arg,
        "ok": ok,
        "sig": "",
    }
    env["sig"] = nostr.sign(digest(env), sk).hex()
    return env


def verify(env: Dict[str, Any]) -> bool:
    try:
        unsigned = dict(env)
        unsigned["sig"] = ""
        return nostr.verify(digest(unsigned), bytes.fromhex(env["pubkey"]), bytes.fromhex(env["sig"]))
    except Exception:
        return False


def parse_secret(text: str) -> bytes:
    """`nsec1…` or 64 hex characters."""
    text = text.strip()
    if len(text) == 64:
        return bytes.fromhex(text)
    parsed = nostr.bech32_decode(text)
    if parsed is None or parsed[0] != "nsec" or len(parsed[1]) != 32:
        raise ValueError("expected nsec1… or 64 hex characters")
    return parsed[1]


def discover(timeout: float = 2.0) -> List[Dict[str, Any]]:
    """Broadcasts the beacon question and collects every answer."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.settimeout(0.3)
    found: Dict[str, Dict[str, Any]] = {}
    deadline = time.time() + timeout
    try:
        for target in ("255.255.255.255", "127.0.0.1"):
            try:
                s.sendto(BEACON_QUERY, (target, BEACON_PORT))
            except OSError:
                pass
        while time.time() < deadline:
            try:
                data, addr = s.recvfrom(1024)
            except socket.timeout:
                continue
            text = data.decode("utf-8", "replace")
            if not text.startswith(BEACON_PREFIX):
                continue
            try:
                info = json.loads(text[len(BEACON_PREFIX):])
            except ValueError:
                continue
            if len(info.get("pubkey", "")) != 64:
                continue
            info["from"] = addr[0]
            found[info["pubkey"]] = info
    finally:
        s.close()
    return list(found.values())


def call(url: str, env: Dict[str, Any], timeout: float = 10.0) -> Dict[str, Any]:
    """POSTs an envelope to a host and returns the verified reply (raises on a bad signature)."""
    base = url.rstrip("/")
    reply = None
    for candidate in (base + "/api/party", base + "/party"):
        req = urllib.request.Request(candidate, data=json.dumps(env).encode("utf-8"), headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                reply = json.loads(r.read().decode("utf-8"))
                break
        except urllib.error.HTTPError as e:
            if e.code in (404, 405):
                continue
            raise
    if reply is None:
        raise RuntimeError("no party endpoint at %s" % base)
    if not verify(reply):
        raise RuntimeError("the host's reply is not signed correctly")
    if reply.get("nonce") != env["nonce"]:
        raise RuntimeError("the host answered a different request")
    return reply


def main(argv: Optional[List[str]] = None) -> int:
    ap = argparse.ArgumentParser(prog="cued-party", description=__doc__.split("\n\n")[0])
    ap.add_argument("--nsec", help="your station key (nsec1… or hex); generated and printed when omitted")
    ap.add_argument("--url", help="host base URL, e.g. http://192.168.1.9:8770; omit to pick the first host found")
    ap.add_argument("op", choices=["discover", "hello", "search", "queue", "now", "skip", "pause", "play"])
    ap.add_argument("value", nargs="?", help="search text or the sha256 to queue")
    a = ap.parse_args(argv)

    if a.nsec:
        sk = parse_secret(a.nsec)
    else:
        sk = nostr.generate_secret()
        print("no --nsec given; using a throwaway key", nostr.public_key(sk).hex(), file=sys.stderr)

    if a.op == "discover":
        hosts = discover()
        if not hosts:
            print("no party hosts answered")
            return 1
        for h in hosts:
            print("%s\t%s\t%s" % (h.get("name", ""), h.get("url", ""), h.get("pubkey", "")))
        return 0

    url = a.url
    if not url:
        hosts = discover()
        if not hosts:
            print("no party hosts answered; pass --url", file=sys.stderr)
            return 1
        url = hosts[0]["url"]
        print("host:", hosts[0].get("name", ""), url, file=sys.stderr)

    arg: Any = None
    if a.op == "search":
        arg = {"q": a.value or ""}
    elif a.op == "queue":
        if not a.value:
            print("queue needs a sha256", file=sys.stderr)
            return 2
        arg = {"sha256": a.value, "title": ""}
    reply = call(url, sign(sk, a.op, arg))
    if not reply.get("ok", True):
        print("error:", (reply.get("arg") or {}).get("error", "request failed"))
        return 1
    print(json.dumps(reply.get("arg"), indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())

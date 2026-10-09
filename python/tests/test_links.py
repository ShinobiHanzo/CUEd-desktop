from cued_companion import links
from cued_companion.links import PairLink, SharePayload, StationLink

PUB = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"
EXPECTED_SHARE = (
    "cued://share?v=1&t=Afterlife%20%28Live%29%20%26%20more&a=Evanescence"
    "&l=https%3A%2F%2Fopen.spotify.com%2Ftrack%2Fabc&f=http%3A%2F%2F192.168.1.5%3A8765%2Ftrack%2F42"
    "&g=rock%2Calt%20metal&b=128.0"
)


def sample():
    return SharePayload(title="Afterlife (Live) & more", artist="Evanescence", link="https://open.spotify.com/track/abc",
                        file_url="http://192.168.1.5:8765/track/42", genres=["rock", "alt metal"], bpm=128.04)


def test_java_style_percent_encoding():
    assert links.enc("a b") == "a%20b"
    assert links.enc("-_.*") == "-_.*"          # URLEncoder keeps these
    assert links.enc("~/+&=") == "%7E%2F%2B%26%3D"  # and encodes these (unlike Python's quote)
    assert links.enc("é") == "%C3%A9"
    assert links.dec("a+b%20c") == "a b c"
    assert links.dec("%C3%A9") == "é"


def test_share_exact_string():
    assert sample().encode() == EXPECTED_SHARE


def test_share_round_trip():
    p = sample()
    d = SharePayload.decode(p.encode())
    assert d is not None
    assert d.title == p.title and d.artist == p.artist and d.link == p.link and d.file_url == p.file_url
    assert d.genres == ["rock", "alt metal"]
    assert d.bpm == 128.0
    assert d.version == 1
    assert d.apk_url is None


def test_share_web_form_decode():
    p = sample()
    web = p.encode_web()
    assert web.startswith("https://shinobihanzo.github.io/CUEd/#v=1&t=Afterlife%20%28Live%29")
    d = SharePayload.decode(web)
    assert d is not None and d.title == p.title and d.file_url == p.file_url
    # case-insensitive base, ? instead of #, surrounding whitespace
    d2 = SharePayload.decode("  HTTPS://shinobihanzo.github.io/CUEd/?" + web.split("#", 1)[1] + " ")
    assert d2 == d


def test_share_minimal_and_invalid():
    assert SharePayload.decode("cued://share?v=1&a=x") is None  # no title
    assert SharePayload.decode("cued://pair?s=00") is None
    assert SharePayload.decode("hello") is None
    d = SharePayload.decode("cued://share?t=Only%20Title")
    assert d == SharePayload(title="Only Title")
    assert SharePayload(title="x", artist="y").encode() == "cued://share?v=1&t=x&a=y"
    d = SharePayload.decode("cued://share?t=x&b=notanumber&v=z&g=,,")
    assert d is not None and d.bpm is None and d.version == 1 and d.genres == []


def test_station_link_round_trip_and_validation():
    s = StationLink(PUB, "Idris & co", ["wss://relay.damus.io", "ws://192.168.1.10:8770/relay"])
    enc = s.encode()
    assert enc == "cued://station?p=%s&n=Idris%%20%%26%%20co&r=wss%%3A%%2F%%2Frelay.damus.io%%2Cws%%3A%%2F%%2F192.168.1.10%%3A8770%%2Frelay" % PUB
    assert StationLink.decode(enc) == s
    assert StationLink(PUB).encode() == "cued://station?p=" + PUB
    assert StationLink.decode("cued://station?p=" + PUB.upper()) == StationLink(PUB)
    assert StationLink.decode("cued://station?p=abc") is None
    assert StationLink.decode("cued://station?n=x") is None


def test_pair_link_round_trip():
    l = PairLink(secret="00112233445566778899aabbccddeeff", name="Idris's laptop",
                 addresses=["http://192.168.1.10:8770", "http://10.8.0.2:8770"],
                 funnel="https://home.example.net:443", cert_sha256="ab" * 32, pubkey=PUB, relay="ws://192.168.1.10:8770/relay")
    s = l.encode()
    assert s.startswith("cued://pair?v=1&n=Idris%27s%20laptop&a=http%3A%2F%2F192.168.1.10%3A8770%2Chttp%3A%2F%2F10.8.0.2%3A8770&f=")
    assert PairLink.decode(s) == l
    assert PairLink.decode("cued://pair?n=x") is None          # no secret
    assert PairLink.decode("cued://pair?s=nothex") is None
    assert PairLink.decode("cued://pair?s=%s&k=zz" % l.secret) is None
    minimal = PairLink.decode("cued://pair?s=" + l.secret.upper())
    assert minimal == PairLink(secret=l.secret)


def test_pair_proof_matches_protocol():
    import hashlib
    import hmac

    secret = "00" * 16
    expected = hmac.new(bytes.fromhex(secret), b"d|n|x", hashlib.sha256).hexdigest()
    assert links.pair_proof(secret, "d", "n", "x") == expected
    assert len(expected) == 64
    assert links.verify_pair_proof(secret, "d", "n", "x", expected.upper())
    assert not links.verify_pair_proof(secret, "d", "other", "x", expected)
    assert not links.verify_pair_proof("zz", "d", "n", "x", expected)
    req = links.build_pair_request(PairLink(secret=secret), "Pixel 8", "0011223344556677", PUB)
    assert req["v"] == 1 and req["pubkey"] == PUB and len(req["nonce"]) == 16
    assert links.verify_pair_proof(secret, req["device"], req["name"], req["nonce"], req["proof"])


def test_decode_link_dispatch():
    assert isinstance(links.decode_link(sample().encode()), SharePayload)
    assert isinstance(links.decode_link(StationLink(PUB).encode()), StationLink)
    assert isinstance(links.decode_link(PairLink(secret="aa" * 16).encode()), PairLink)
    assert links.decode_link("spotify:track:xyz") is None


# ---------------------------------------------------------------- friends (§9) and friend requests in shares


def test_friend_link_round_trip_matches_rust():
    from cued_companion.links import FriendLink

    l = FriendLink("ab" * 32, "Idris", ["wss://nos.lol"])
    s = l.encode()
    assert s == "cued://friend?p=%s&n=Idris&r=wss%%3A%%2F%%2Fnos.lol" % ("ab" * 32)
    assert FriendLink.decode(s) == l
    assert FriendLink.decode("cued://friend?p=zz") is None
    assert FriendLink.decode("cued://friend?n=x") is None
    assert FriendLink.decode("cued://station?p=" + PUB) is None
    assert FriendLink.decode("cued://friend?p=" + PUB.upper()) == FriendLink(PUB)
    assert FriendLink(PUB, "  ").encode() == "cued://friend?p=" + PUB  # blank name omitted
    assert isinstance(links.decode_link(s), FriendLink)
    assert links.decode_link(s) == l
    # the kind-30778 event shape
    assert links.FRIEND_KIND == 30778
    assert links.friend_tags(PUB) == [["d", "friend:" + PUB], ["p", PUB], ["t", "cued-friend"]]
    assert links.friend_filter(PUB) == {"kinds": [30778], "#p": [PUB]}
    assert links.friend_filter(PUB, since=5)["since"] == 5


def test_share_carries_friend_request():
    p = SharePayload(title="Song", artist="Band", friend_pubkey="ab" * 32, friend_name="Idris")
    s = p.encode()
    assert s == "cued://share?v=1&t=Song&a=Band&fr=%s&fn=Idris" % ("ab" * 32)
    # fr/fn come after b when both are present
    full = sample()
    full.friend_pubkey = "ab" * 32
    full.friend_name = "Idris"
    assert full.encode() == EXPECTED_SHARE + "&fr=%s&fn=Idris" % ("ab" * 32)
    d = SharePayload.decode(full.encode())
    assert d.friend_pubkey == "ab" * 32 and d.friend_name == "Idris" and d.title == full.title and d.bpm == 128.0
    # web form carries it too
    assert SharePayload.decode(full.encode_web()).friend_pubkey == "ab" * 32
    # fn without fr is not emitted; fr alone is
    assert SharePayload(title="x", friend_name="n").encode() == "cued://share?v=1&t=x&a="
    assert SharePayload(title="x", friend_pubkey="ab" * 32).encode() == "cued://share?v=1&t=x&a=&fr=" + "ab" * 32
    # a bad key is dropped rather than failing the whole share (share.rs does the same); the name stays
    d = SharePayload.decode("cued://share?t=x&a=y&fr=zz&fn=Who")
    assert d is not None and d.friend_pubkey is None and d.friend_name == "Who" and d.title == "x"
    d = SharePayload.decode("cued://share?t=x&a=y&fr=" + ("AB" * 32))
    assert d.friend_pubkey == "ab" * 32
    # plain shares are unchanged
    assert SharePayload.decode(sample().encode()).friend_pubkey is None
    assert sample().encode() == EXPECTED_SHARE

"""
CUEd desktop companion, Python side.

Three roles live in this package:

* ``cued_companion.server``    the spotdl download worker ``cuedd`` spawns
                               (same JSON API the phone's Companion backend
                               already speaks).
* ``cued_companion.links`` and ``cued_companion.nostr``
                               pure-Python codecs for ``cued://`` links and the
                               secp256k1 / NIP-01 / NIP-19 pieces a station
                               needs, bit-for-bit compatible with the phone.
* ``cued_companion.phone_sim`` a command-line stand-in for the phone so the
                               desktop can be driven end to end without Android.

Standard library only at runtime; ``websockets`` is imported lazily by the
station publisher and is optional.
"""

__version__ = "0.1.0"

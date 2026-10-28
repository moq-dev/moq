"""Publisher epochs: the UUIDv7 text a route carries to name one publisher run."""

from __future__ import annotations

from moq_ffi import moq_mint_epoch


def mint_epoch() -> str:
    """Mint a fresh epoch from the wall clock and secure randomness, ordered newest last.

    Mint one per publisher run and announce it with ``AnnounceRoute(epoch=...)``, so a
    restart reads as a new broadcast to viewers instead of a stalled one.
    """
    return moq_mint_epoch()

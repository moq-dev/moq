"""Publisher epochs: the UUIDv7 text a route carries to name one publisher run."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

from moq_ffi import moq_epoch_time_ms, moq_mint_epoch


def mint_epoch() -> str:
    """Mint a fresh epoch from the wall clock and secure randomness, ordered newest last.

    Mint one per publisher run and announce it with ``Route(epoch=...)``, so a
    restart reads as a new broadcast to viewers instead of a stalled one.
    """
    return moq_mint_epoch()


def epoch_time(epoch: str) -> datetime:
    """The UTC wall-clock time an epoch encodes, to the millisecond.

    Raises `moq.Error` unless ``epoch`` is a lowercase hyphenated UUIDv7.
    """
    return datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(milliseconds=moq_epoch_time_ms(epoch))

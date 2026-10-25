"""The breaks between decoded frames, shared by :mod:`moq.audio` and :mod:`moq.video`."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from ._records import _from_us


@dataclass(frozen=True)
class Gap:
    """Media after what was already read was lost: a group was skipped or never arrived.

    What follows does not continue what came before. ``duration`` is an upper bound
    on how long until the next frame resumes, starting at ``timestamp``, the
    furthest presentation point already read.
    """

    timestamp: timedelta
    duration: timedelta


@dataclass(frozen=True)
class End:
    """The publisher stopped its encoder: it paused, switched source, or finished.

    Frames that follow and start before ``end`` are the stopped encoder's
    remaining media; the first at or past it comes from a fresh encoder.
    """

    end: timedelta


def _gap(timestamp_us: int, duration: timedelta) -> Gap:
    return Gap(timestamp=_from_us(timestamp_us), duration=duration)


def _end(end_us: int) -> End:
    return End(end=_from_us(end_us))

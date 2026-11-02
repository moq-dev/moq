"""The breaks between decoded frames, shared by encoded and decoded media consumers."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from moq_ffi import MoqTimestamp

from ._records import Timestamp


@dataclass(frozen=True)
class Gap:
    """Media after what was already read was lost: a group was skipped or never arrived.

    What follows does not continue what came before. ``duration`` is an upper bound
    on how long until the next frame resumes, starting at ``timestamp``, the
    furthest presentation point already read.
    """

    timestamp: Timestamp
    duration: timedelta


@dataclass(frozen=True)
class End:
    """The publisher stopped its encoder: it paused, switched source, or finished.

    Frames that follow and start before ``end`` are the stopped encoder's
    remaining media; the first at or past it comes from a fresh encoder.
    """

    end: Timestamp


def _gap(timestamp: MoqTimestamp, duration: timedelta) -> Gap:
    return Gap(timestamp=Timestamp._from_ffi(timestamp), duration=duration)


def _end(end: MoqTimestamp) -> End:
    return End(end=Timestamp._from_ffi(end))

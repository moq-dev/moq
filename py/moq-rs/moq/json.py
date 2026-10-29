"""JSON tracks, mirroring the ``moq-json`` crate.

Each type wraps a track. A producer takes over a :class:`~moq.TrackProducer`; the snapshot and
stream producers also advertise it in their broadcast's catalog (``json.tracks.<name>``) for as
long as they live, while the catalog has no window mode. A consumer takes over a
:class:`~moq.TrackConsumer` that has not read a group yet. Either track can come from a request
as well as by name. Values are any JSON-serializable Python object.

Compression is the track's: publish it with ``TrackInfo(compress=True)``, which JSON benefits from,
and a consumer reads it with no option of its own.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import timedelta
from typing import Any

from moq_ffi import (
    MoqJsonSnapshotConfig,
    MoqJsonSnapshotConsumer,
    MoqJsonSnapshotProducer,
    MoqJsonStreamConsumer,
    MoqJsonStreamProducer,
    MoqJsonValue,
    MoqJsonWindowConfig,
    MoqJsonWindowConsumer,
    MoqJsonWindowEvent,
    MoqJsonWindowProducer,
    MoqJsonWindowTimed,
)

from . import publish, subscribe
from ._records import _opt_from_us, _opt_us
from ._stream import _Stream
from .publish import BroadcastProducer, TrackDemand

__all__ = [
    "SnapshotConsumer",
    "SnapshotProducer",
    "StreamConsumer",
    "StreamProducer",
    "Timed",
    "WindowConsumer",
    "WindowEvent",
    "WindowPop",
    "WindowProducer",
    "WindowPush",
    "WindowSkip",
]


@dataclass(frozen=True)
class Timed:
    """A parsed JSON value and the timestamp of the frame that carried it.

    ``timestamp`` is on the broadcast clock, and ``None`` on an untimed track. It is
    rounded down to a whole microsecond, so a tick on a track like 90 kHz may not
    survive a read and republish exactly.
    """

    value: Any
    timestamp: timedelta | None

    @staticmethod
    def _from_ffi(value: MoqJsonValue | None) -> Timed | None:
        if value is None:
            return None
        return Timed(value=json.loads(value.value), timestamp=_opt_from_us(value.timestamp_us))


class SnapshotProducer:
    """Publish a JSON value that consumers see as a single latest state (lossy).

    Each :meth:`update` supersedes the last; a late joiner only sees the newest value, encoded as
    snapshots and merge-patch deltas automatically. Set ``deltas=False`` to publish every update
    as a full snapshot in its own group. With deltas on, give the track a ``refresh`` ratio so a
    join after more than that many snapshots' worth of deltas asks for the value restated in a
    fresh group. A name the catalog already carries is refused.
    """

    def __init__(
        self,
        broadcast: BroadcastProducer,
        track: publish.TrackProducer,
        *,
        deltas: bool = True,
    ) -> None:
        config = MoqJsonSnapshotConfig(deltas=deltas)
        self._inner = MoqJsonSnapshotProducer(broadcast._inner, track._inner, config)

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this track has subscribers."""
        return TrackDemand(self._inner.demand())

    def update(self, value: Any, *, timestamp: timedelta | None = None) -> None:
        """Publish a new value. A no-op if unchanged from the previous update."""
        self._inner.update(json.dumps(value), _opt_us(timestamp, "timestamp"))

    def finish(self) -> None:
        """Finish the track, closing any open group."""
        self._inner.finish()


class StreamProducer:
    """Publish an ordered log of JSON records (lossless).

    Every :meth:`append` is preserved and delivered in order. A name the catalog already carries is
    refused.
    """

    def __init__(self, broadcast: BroadcastProducer, track: publish.TrackProducer) -> None:
        self._inner = MoqJsonStreamProducer(broadcast._inner, track._inner)

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this track has subscribers."""
        return TrackDemand(self._inner.demand())

    def append(self, value: Any, *, timestamp: timedelta | None = None) -> None:
        """Append one record to the log."""
        self._inner.append(json.dumps(value), _opt_us(timestamp, "timestamp"))

    def finish(self) -> None:
        """Finish the track, closing the group."""
        self._inner.finish()


class SnapshotConsumer(_Stream[Timed]):
    """Async iterator over a JSON snapshot track, yielding each state in order (lossy).

    Each item is a :class:`Timed`. Groups older than the subscription's ``max_delay`` are skipped,
    so a playhead reader sets that budget to its distance behind live; :meth:`latest` skips to the
    newest state instead. Usable as an async context manager that cancels on exit.
    """

    def __init__(self, track: subscribe.TrackConsumer) -> None:
        self._inner = MoqJsonSnapshotConsumer(track._inner)

    async def _next(self) -> Timed | None:
        return Timed._from_ffi(await self._inner.next())

    async def latest(self) -> Timed | None:
        """Skip to the newest state, or wait for one. ``None`` once the track ends.

        Skipped states are gone; iteration continues after this one.
        """
        return Timed._from_ffi(await self._inner.latest())

    def cancel(self) -> None:
        """Cancel all current and future reads."""
        self._inner.cancel()


class StreamConsumer(_Stream[Timed]):
    """Async iterator over a JSON stream track, yielding every record in order (lossless).

    Each item is a :class:`Timed`. Usable as an async context manager that cancels on exit.
    """

    def __init__(self, track: subscribe.TrackConsumer) -> None:
        self._inner = MoqJsonStreamConsumer(track._inner)

    async def _next(self) -> Timed | None:
        return Timed._from_ffi(await self._inner.next())

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()


class WindowProducer:
    """Publish a sliding window of JSON records: pushed at the back, popped from the front.

    Unlike a stream, which keeps every record in one group forever, a window rolls to new groups
    that restate the records it retains, so a reader can join at any point. Bound what it retains
    by popping, or with ``checkpoint_records`` for a window that is never popped: a reader that
    keeps up still sees every record, while one joining later is told the older ones were skipped.
    Give the track a ``refresh`` ratio so a join past it gets the window restated in a fresh group.

    Not advertised in the broadcast's catalog, which has no window mode; a reader names the track.
    """

    def __init__(
        self,
        track: publish.TrackProducer,
        *,
        checkpoint_records: int | None = None,
    ) -> None:
        config = MoqJsonWindowConfig(checkpoint_records=checkpoint_records)
        self._inner = MoqJsonWindowProducer(track._inner, config)

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this track has subscribers."""
        return TrackDemand(self._inner.demand())

    def push(self, value: Any, *, timestamp: timedelta | None = None) -> None:
        """Append one record to the back of the window."""
        self._inner.push(json.dumps(value), _opt_us(timestamp, "timestamp"))

    def pop(self, count: int, *, timestamp: timedelta | None = None) -> None:
        """Drop ``count`` records from the front of the window, clamped to what it holds."""
        self._inner.pop(count, _opt_us(timestamp, "timestamp"))

    def cut(self) -> None:
        """Close the open group, so the next edit opens one restating the retained window.

        A no-op when no group is open. Cut at a natural boundary, such as the end of an utterance,
        so every edit so far sits in a group no later frame extends.
        """
        self._inner.cut()

    def finish(self) -> None:
        """Finish the track, closing any open group."""
        self._inner.finish()


@dataclass(frozen=True)
class WindowPush:
    """A record joined the window at absolute ``index``."""

    index: int
    value: Any


@dataclass(frozen=True)
class WindowPop:
    """The records at these absolute indices left the window."""

    range: range


@dataclass(frozen=True)
class WindowSkip:
    """The records at these absolute indices will never be delivered.

    They were pushed and dropped while this reader was behind, or fell outside the producer's
    checkpoint when it joined.
    """

    range: range


WindowEvent = WindowPush | WindowPop | WindowSkip
"""One change to a window, as a :class:`WindowConsumer` sees it."""


def _window_event(timed: MoqJsonWindowTimed | None) -> Timed | None:
    if timed is None:
        return None
    event = timed.event
    value: WindowEvent
    if isinstance(event, MoqJsonWindowEvent.PUSH):
        value = WindowPush(index=event.index, value=json.loads(event.value))
    elif isinstance(event, MoqJsonWindowEvent.POP):
        value = WindowPop(range(event.start, event.end))
    elif isinstance(event, MoqJsonWindowEvent.SKIP):
        value = WindowSkip(range(event.start, event.end))
    else:
        raise TypeError(f"unknown window event: {event!r}")
    return Timed(value=value, timestamp=_opt_from_us(timed.timestamp_us))


class WindowConsumer(_Stream[Timed]):
    """Async iterator over a JSON window track, yielding each change in order.

    Each item is a :class:`Timed` whose ``value`` is a :class:`WindowPush`, :class:`WindowPop`, or
    :class:`WindowSkip`. Group rolls never surface: a restated record this reader already has
    yields nothing. A record first learned of from a restatement (after a join, or while behind)
    carries that frame's timestamp, not its original push time. Usable as an async context manager
    that cancels on exit.
    """

    def __init__(self, track: subscribe.TrackConsumer) -> None:
        self._inner = MoqJsonWindowConsumer(track._inner)

    async def _next(self) -> Timed | None:
        return _window_event(await self._inner.next())

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()

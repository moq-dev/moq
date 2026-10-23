"""JSON tracks, mirroring the ``moq-json`` crate.

Each type wraps a track. A producer takes over a :class:`~moq.TrackProducer` and advertises it
in its broadcast's catalog (``json.tracks.<name>``) for as long as it lives; a consumer takes over
a :class:`~moq.TrackConsumer` that has not read a group yet. Either track can come from a request
as well as by name. Values are any JSON-serializable Python object.
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
    MoqJsonStreamConfig,
    MoqJsonStreamConsumer,
    MoqJsonStreamProducer,
    MoqJsonValue,
)

from . import publish, subscribe
from ._records import _opt_from_us, _opt_us
from ._stream import _Stream
from .publish import BroadcastProducer, TrackDemand

__all__ = ["SnapshotConsumer", "SnapshotProducer", "StreamConsumer", "StreamProducer", "Timed"]


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
    fresh group. Set ``compression`` to DEFLATE-compress each group; the consumer
    must pass the same flag. A name the catalog already carries is refused.
    """

    def __init__(
        self,
        broadcast: BroadcastProducer,
        track: publish.TrackProducer,
        *,
        deltas: bool = True,
        compression: bool = False,
    ) -> None:
        config = MoqJsonSnapshotConfig(deltas=deltas, compression=compression)
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

    Every :meth:`append` is preserved and delivered in order. Set ``compression`` to
    DEFLATE-compress the group; the consumer must pass the same flag. A name the catalog already
    carries is refused.
    """

    def __init__(
        self, broadcast: BroadcastProducer, track: publish.TrackProducer, *, compression: bool = False
    ) -> None:
        config = MoqJsonStreamConfig(compression=compression)
        self._inner = MoqJsonStreamProducer(broadcast._inner, track._inner, config)

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
    newest state instead. Pass the same ``compression`` the producer used. Usable as an async
    context manager that cancels on exit.
    """

    def __init__(self, track: subscribe.TrackConsumer, *, compression: bool = False) -> None:
        # deltas is producer-only, so leave it at its default here.
        config = MoqJsonSnapshotConfig(compression=compression)
        self._inner = MoqJsonSnapshotConsumer(track._inner, config)

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

    Each item is a :class:`Timed`. Pass the same ``compression`` the producer used. Usable as an
    async context manager that cancels on exit.
    """

    def __init__(self, track: subscribe.TrackConsumer, *, compression: bool = False) -> None:
        config = MoqJsonStreamConfig(compression=compression)
        self._inner = MoqJsonStreamConsumer(track._inner, config)

    async def _next(self) -> Timed | None:
        return Timed._from_ffi(await self._inner.next())

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()

"""JSON tracks, mirroring the ``moq-json`` crate.

Each type wraps a track. A producer takes over a :class:`~moq.TrackProducer` and advertises it
in its broadcast's catalog (``json.tracks.<name>``) for as long as it lives; a consumer takes over
a :class:`~moq.TrackConsumer` that has not read a group yet. Either track can come from a request
as well as by name. Values are any JSON-serializable Python object.
"""

from __future__ import annotations

import json
from typing import Any

from moq_ffi import (
    MoqJsonSnapshotConfig,
    MoqJsonSnapshotConsumer,
    MoqJsonSnapshotProducer,
    MoqJsonStreamConfig,
    MoqJsonStreamConsumer,
    MoqJsonStreamProducer,
)

from .publish import BroadcastProducer, TrackDemand, TrackProducer
from .subscribe import TrackConsumer

__all__ = ["SnapshotConsumer", "SnapshotProducer", "StreamConsumer", "StreamProducer"]


class SnapshotProducer:
    """Publish a JSON value that consumers see as a single latest state (lossy).

    Each :meth:`update` supersedes the last; a late joiner only sees the newest value, encoded as
    snapshots and merge-patch deltas automatically. ``delta_ratio`` controls how aggressively
    deltas replace full snapshots (0 disables them); ``None`` uses the binding's default. Set
    ``compression`` to DEFLATE-compress each group; the consumer must pass the same flag. A name
    the catalog already carries is refused.
    """

    def __init__(
        self,
        broadcast: BroadcastProducer,
        track: TrackProducer,
        *,
        delta_ratio: int | None = None,
        compression: bool = False,
    ) -> None:
        # Let the record supply delta_ratio's default rather than restating it here.
        config = (
            MoqJsonSnapshotConfig(compression=compression)
            if delta_ratio is None
            else MoqJsonSnapshotConfig(delta_ratio=delta_ratio, compression=compression)
        )
        self._inner = MoqJsonSnapshotProducer(broadcast._inner, track._inner, config)

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this track has subscribers."""
        return TrackDemand(self._inner.demand())

    def update(self, value: Any) -> None:
        """Publish a new value. A no-op if unchanged from the previous update."""
        self._inner.update(json.dumps(value))

    def finish(self) -> None:
        """Finish the track, closing any open group."""
        self._inner.finish()


class StreamProducer:
    """Publish an ordered log of JSON records (lossless).

    Every :meth:`append` is preserved and delivered in order. Set ``compression`` to
    DEFLATE-compress the group; the consumer must pass the same flag. A name the catalog already
    carries is refused.
    """

    def __init__(self, broadcast: BroadcastProducer, track: TrackProducer, *, compression: bool = False) -> None:
        config = MoqJsonStreamConfig(compression=compression)
        self._inner = MoqJsonStreamProducer(broadcast._inner, track._inner, config)

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this track has subscribers."""
        return TrackDemand(self._inner.demand())

    def append(self, value: Any) -> None:
        """Append one record to the log."""
        self._inner.append(json.dumps(value))

    def finish(self) -> None:
        """Finish the track, closing the group."""
        self._inner.finish()


class SnapshotConsumer:
    """Async iterator over a JSON snapshot track, yielding the latest value (lossy).

    Each item is a parsed Python object. A consumer that has fallen behind collapses the backlog
    and yields only the latest value. Pass the same ``compression`` the producer used. Usable as
    an async context manager that cancels on exit.
    """

    def __init__(self, track: TrackConsumer, *, compression: bool = False) -> None:
        # delta_ratio is producer-only, so leave it at its default here.
        config = MoqJsonSnapshotConfig(compression=compression)
        self._inner = MoqJsonSnapshotConsumer(track._inner, config)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Any:
        value = await self._inner.next()
        if value is None:
            raise StopAsyncIteration
        return json.loads(value)

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()


class StreamConsumer:
    """Async iterator over a JSON stream track, yielding every record in order (lossless).

    Each item is a parsed Python object. Pass the same ``compression`` the producer used. Usable
    as an async context manager that cancels on exit.
    """

    def __init__(self, track: TrackConsumer, *, compression: bool = False) -> None:
        config = MoqJsonStreamConfig(compression=compression)
        self._inner = MoqJsonStreamConsumer(track._inner, config)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Any:
        value = await self._inner.next()
        if value is None:
            raise StopAsyncIteration
        return json.loads(value)

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()

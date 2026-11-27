"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

from typing import TYPE_CHECKING

from moq_ffi import (
    MoqBroadcastProducer,
    MoqBroadcastRequests,
    MoqFill,
    MoqGroupProducer,
    MoqOrder,
    MoqTrackDemand,
    MoqTrackProducer,
    MoqTrackRequest,
    MoqTrackRequests,
)

from ._records import _subscription, _track_info
from ._stream import _Stream
from .types import (
    DatagramInfo,
    Frame,
    Position,
    Subscription,
    Timestamp,
    TrackInfo,
)

if TYPE_CHECKING:
    from .datagram import DatagramProducer, DatagramRequests
    from .subscribe import BroadcastConsumer, GroupConsumer, TrackConsumer


class GroupProducer:
    """Writes frames into a single group on a track."""

    def __init__(self, inner: MoqGroupProducer) -> None:
        self._inner = inner

    @property
    def start(self) -> int:
        """The start of this group within the track."""
        return self._inner.start()

    def consume(self) -> GroupConsumer:
        """Create a consumer that reads frames from this group."""
        from .subscribe import GroupConsumer

        return GroupConsumer(self._inner.consume())

    async def ready(self, size: int) -> None:
        """Wait until a frame of ``size`` bytes fits a window ahead of the fastest reader.

        Await it before each :meth:`write_frame` so that reader paces the writer,
        as TCP backpressure does, instead of the group shedding frames it has not
        taken, which fails it with ``moq.Error.Evicted``; slower readers are not
        waited on. It also
        waits until a reader holds the group, and resolves once the group is closed
        so the next write raises why. Raises ``moq.Error.Closed`` after :meth:`abort`.
        """
        await self._inner.ready(size)

    def write_frame(self, payload: bytes, timestamp: Timestamp) -> None:
        """Write a frame with its presentation timestamp.

        On a scale-less track the timestamp is ``Timestamp.unscaled(self.start)``.

        Never blocks: without :meth:`ready`, a writer that outpaces its readers
        sheds the front of the group instead.
        """
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def finish(self) -> None:
        """Close this group cleanly, marking it complete for subscribers.

        The handle remains so :meth:`abort` can still run.
        """
        self._inner.finish()

    def finish_at(self, end: int) -> None:
        """Close this group cleanly with its exclusive ``end`` in the track's timescale.

        No other group may start before ``end``. The handle remains so :meth:`abort` can still
        run.
        """
        self._inner.finish_at(end)

    def abort(self, error_code: int) -> None:
        """Abort this group with an application error code."""
        self._inner.abort(error_code)


class TrackDemand:
    """A watch-only handle to whether a published track has subscribers, and when they ask for a new group.

    Returned by a producer's ``demand()``. Weak: holding it neither keeps the
    track open nor locks the producer, so a wait can park here while the
    producer keeps publishing. Waits raise ``moq.Error.Closed`` once the track
    is released.
    """

    def __init__(self, inner: MoqTrackDemand) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The name of the track this watches."""
        return self._inner.name()

    def wanted(self) -> bool:
        """Whether anyone consumes the track: a subscription, fetch, or a group one of them is still reading."""
        return self._inner.wanted()

    async def wanted_changed(self) -> bool:
        """Wait for ``wanted`` to change and return it. The first call returns at once.

        End a track nobody wants by dropping every reference to its producer (``del producer``)
        once this returns ``False``: a reader that joined since is served by the next request
        for the track while a request handler is live.
        """
        return await self._inner.wanted_changed()

    async def refresh(self) -> None:
        """Wait until a subscriber asks for a new group.

        That is the first group of a track with none, or a join judged by the track's
        :class:`TrackRefresh` once its interval passed. Returns at once while the request is
        pending, and the next group written answers it, so loop: wait here, then write a group
        that starts with a keyframe or a snapshot.
        """
        await self._inner.refresh()


class TrackProducer:
    """Track producer: write arbitrary byte payloads with no codec required.

    Same pattern as moq-boy's status/command tracks.
    """

    def __init__(self, inner: MoqTrackProducer) -> None:
        self._inner = inner

    def demand(self) -> TrackDemand:
        """A watch-only handle to this track's name and whether it has subscribers."""
        return TrackDemand(self._inner.demand())

    def requests(self) -> TrackRequests:
        """Serve fetches for groups that are not currently cached."""
        return TrackRequests(self._inner.requests())

    def append_group(self, start: Timestamp) -> GroupProducer:
        """Start the next group at ``start``; write frames into it, then finish().

        ``start`` is converted to the track's timescale; on a scale-less track it is
        :meth:`Timestamp.unscaled` at the group's key. A start at or before a group the
        track holds is refused.
        """
        return GroupProducer(self._inner.append_group(start._ffi()))

    def create_group(self, start: int) -> GroupProducer:
        """Create a group with an explicit start."""
        return GroupProducer(self._inner.create_group(start))

    def skip_groups(self, start: int, end: int) -> None:
        """Declare that groups ``start`` up to the exclusive ``end`` will never be produced.

        Readers move past them, and creating a group there then fails.
        """
        self._inner.skip_groups(start, end)

    def write_frame(self, payload: bytes, timestamp: Timestamp) -> None:
        """Write a single-frame group with its presentation timestamp.

        The group starts at the frame's time; on a scale-less track the timestamp is
        :meth:`Timestamp.unscaled` at the new group's key instead.
        """
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def largest(self) -> Position | None:
        """The newest group written, at its newest frame; ``None`` before any."""
        return self._inner.largest()

    def consume(self, subscription: Subscription | None = None) -> TrackConsumer:
        """Create a consumer that reads directly from this producer's track.

        ``subscription`` tunes delivery priority, group range, and staleness; omit for defaults.
        """
        from .subscribe import TrackConsumer

        return TrackConsumer(self._inner.consume(_subscription(subscription)))

    def abort(self, error_code: int) -> None:
        """Abort this track with an application error code."""
        self._inner.abort(error_code)

    def finish(self) -> None:
        """Finish publishing and flush a clean end to subscribers.

        The handle remains so :meth:`abort` can still run.
        """
        self._inner.finish()

    def finish_at(self, final_start: int) -> None:
        """Declare the exclusive final group start ahead of the live edge."""
        self._inner.finish_at(final_start)


class TrackRequest:
    """A subscriber-requested track that hasn't been accepted yet.

    Accept it for raw writes, hand it to :class:`~moq.media.Requested` for :meth:`~moq.media.TrackProducer.audio`
    or :meth:`~moq.media.TrackProducer.video` to publish media (the importer
    accepts it), or reject it to fail the subscriber.
    """

    def __init__(self, inner: MoqTrackRequest) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The requested track name."""
        return self._inner.name()

    def accept(self, info: TrackInfo | None = None) -> TrackProducer:
        """Accept the request as a raw track.

        ``info`` fixes the track's timescale, priority, and cache; omit for defaults. A track asked
        for again, because its producer was released while read, must be accepted with the info it
        was published with: another ends the track, and the producer's writes raise.
        """
        return TrackProducer(self._inner.accept(_track_info(info)))

    def demand(self) -> TrackDemand:
        """Watch this track's subscribers, including after accepting it."""
        return TrackDemand(self._inner.demand())

    def requests(self) -> TrackRequests:
        """Create a fetch handler before accepting this requested track."""
        return TrackRequests(self._inner.requests())

    def reject(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.reject(error_code)


class Fill:
    """A run of groups readers miss, for a :class:`TrackRequests` to serve.

    Call :meth:`accept` for each group you have and :meth:`drop_groups` for the rest, one
    group at a time as :meth:`wanted_changed` yields them; :meth:`wanted` names what is still
    owed, in the fill's order. Dropping the fill names
    whatever is left as dropped while a reader still wants it; once nobody does, it is
    withdrawn, so stop serving it once :meth:`wanted` is ``None``. Stop serving a single group
    when :meth:`unwanted` returns for it, and move on to :meth:`wanted`. Serve the fills you
    take concurrently: each waits on its own readers.
    """

    def __init__(self, inner: MoqFill) -> None:
        self._inner = inner

    @property
    def start(self) -> Position:
        """The first group to fill, and the frame within it to start from."""
        return self._inner.start()

    @property
    def end(self) -> int:
        """The exclusive end group."""
        return self._inner.end()

    @property
    def priority(self) -> int:
        """The most urgent delivery priority among the readers waiting on it."""
        return self._inner.priority()

    @property
    def order(self) -> MoqOrder:
        """Which end to fill first."""
        return self._inner.order()

    def wanted(self) -> int | None:
        """The next group still owed in the fill's order.

        ``None`` once every group is answered or no reader waits on the fill. It does not wait
        for the readers; :meth:`wanted_changed` does.
        """
        return self._inner.wanted()

    async def wanted_changed(self) -> int | None:
        """The next group to serve, once every group this fill accepted reached a reader.

        Returns ``wanted`` once it changed and a reader picked up what was served, as it next
        reads. ``None``, once the fill is done, never waits; the first call returns at once
        unless an accepted group has yet to reach a reader.
        """
        return await self._inner.wanted_changed()

    def accept(self, start: int) -> GroupProducer:
        """Insert group ``start`` and return a producer for it.

        The first group starts at :attr:`start`'s frame, so its frames keep their indices.
        """
        return GroupProducer(self._inner.accept(start))

    def drop_groups(self, start: int, end: int) -> None:
        """Name groups ``start`` up to the exclusive ``end`` as never to be delivered."""
        self._inner.drop_groups(start, end)

    def is_wanted(self, start: int) -> bool:
        """Whether a reader still waits for group ``start`` from this fill.

        False once every reader took it or narrowed past it, or nobody waits on the fill.
        """
        return self._inner.is_wanted(start)

    async def unwanted(self, start: int) -> None:
        """Wait until no reader waits for group ``start`` from this fill, so stop serving it."""
        await self._inner.unwanted(start)


class TrackRequests(_Stream[Fill]):
    """Async source of the fills readers of one track need: runs of groups the cache lacks.

    Usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqTrackRequests) -> None:
        self._inner = inner

    async def _next(self) -> Fill:
        return await self.requested_fill()

    async def requested_fill(self) -> Fill:
        """Wait for the next fill."""
        return Fill(await self._inner.requested_fill())

    def cancel(self) -> None:
        """Cancel current and future fill waits."""
        self._inner.cancel()


class BroadcastRequests(_Stream[TrackRequest]):
    """Async source of tracks requested by subscribers.

    Hold this object while subscriptions to unknown tracks should be accepted.
    Usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqBroadcastRequests) -> None:
        self._inner = inner

    async def _next(self) -> TrackRequest:
        return await self.requested_track()

    async def requested_track(self) -> TrackRequest:
        """Await the next track a subscriber requested but that isn't published yet."""
        return TrackRequest(await self._inner.requested_track())

    def datagrams(self) -> DatagramRequests:
        """Serve on-demand datagram tracks through their own request queue."""
        from .datagram import DatagramRequests

        return DatagramRequests(self._inner.datagrams())

    def cancel(self) -> None:
        """Stop accepting track requests and release the underlying handle."""
        self._inner.cancel()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface.

    It fills tracks; announcing belongs to the :class:`moq.Route` that
    :meth:`moq.OriginProducer.route` returns, the way to publish at a path.
    Constructing one directly creates a standalone broadcast for local pub/sub.
    """

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    @classmethod
    def _from_inner(cls, inner: MoqBroadcastProducer) -> BroadcastProducer:
        """Wrap an existing FFI producer (e.g. one created from an origin)."""
        self = cls.__new__(cls)
        self._inner = inner
        return self

    def requests(self) -> BroadcastRequests:
        """Accept subscriptions to tracks that are not published yet."""
        return BroadcastRequests(self._inner.requests())

    def create_datagrams(self, name: str, info: DatagramInfo | None = None) -> DatagramProducer:
        """Create a live datagram track, independent of stream tracks with the same name."""
        from .datagram import DatagramProducer

        return DatagramProducer(self._inner.create_datagrams(name, info))

    def publish_track(self, name: str, info: TrackInfo | None = None) -> TrackProducer:
        """Create a track. Send any bytes, no codec validation. ``info`` sets track
        properties (priority, cache, timescale); omit for defaults."""
        return TrackProducer(self._inner.publish_track(name, _track_info(info)))

    def consume(self) -> BroadcastConsumer:
        """Create a consumer that reads from this broadcast's tracks."""
        from .subscribe import BroadcastConsumer

        return BroadcastConsumer(self._inner.consume())

    def close(self) -> None:
        """End the broadcast for good: retract it and serve no new tracks.

        Tracks still open on it end with ``Unroutable`` (``Internal`` to a moq-transport reader),
        and their later calls raise ``moq.Error.Closed``; finish a track first to end it cleanly.
        Closing again is a no-op.
        """
        self._inner.close()

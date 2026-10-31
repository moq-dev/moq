"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

from datetime import timedelta
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
    Frame,
    Subscription,
    TrackInfo,
)

if TYPE_CHECKING:
    from .subscribe import BroadcastConsumer, GroupConsumer, TrackConsumer


class GroupProducer:
    """Writes frames into a single group on a track."""

    def __init__(self, inner: MoqGroupProducer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    def consume(self) -> GroupConsumer:
        """Create a consumer that reads frames from this group."""
        from .subscribe import GroupConsumer

        return GroupConsumer(self._inner.consume())

    def write_frame(self, payload: bytes, timestamp: timedelta | None = None) -> None:
        """Write a frame with its presentation timestamp."""
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def finish(self) -> None:
        """Close this group cleanly, marking it complete for subscribers.

        The handle remains so :meth:`abort` can still run.
        """
        self._inner.finish()

    def abort(self, error_code: int) -> None:
        """Abort this group with an application error code."""
        self._inner.abort(error_code)


class TrackDemand:
    """A watch-only handle to the group a published track's subscribers want next.

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

    def wanted(self) -> int | None:
        """The group subscribers want next, or None while nobody subscribes.

        A value past the newest group asks for a new group now, typically a keyframe.
        """
        return self._inner.wanted()

    async def wanted_changed(self) -> int | None:
        """Wait for ``wanted`` to change and return it. The first call returns at once."""
        return await self._inner.wanted_changed()


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

    def append_group(self) -> GroupProducer:
        """Start a new group; write frames into it, then finish()."""
        return GroupProducer(self._inner.append_group())

    def create_group(self, sequence: int) -> GroupProducer:
        """Create a group with an explicit sequence number."""
        return GroupProducer(self._inner.create_group(sequence))

    def write_frame(self, payload: bytes, timestamp: timedelta | None = None) -> None:
        """Write a single-frame group with its presentation timestamp."""
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def append_datagram(self, payload: bytes, timestamp: timedelta | None = None) -> int:
        """Send a best-effort datagram and return its sequence number.

        Payloads are capped at 1200 bytes. Datagram delivery requires a datagram-capable
        transport and wire version; there is no stream fallback.
        """
        return self._inner.append_datagram(Frame(payload, timestamp)._ffi())

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

    def finish_at(self, final_sequence: int) -> None:
        """Declare the exclusive final group sequence ahead of the live edge."""
        self._inner.finish_at(final_sequence)


class TrackRequest:
    """A subscriber-requested track that hasn't been accepted yet.

    Accept it for raw writes, hand it to :class:`~moq.media.Requested` for :meth:`~moq.media.TrackProducer.audio`
    or :meth:`~moq.media.TrackProducer.video` to publish media (the importer
    accepts it), or abort it to reject the subscriber.
    """

    def __init__(self, inner: MoqTrackRequest) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The requested track name."""
        return self._inner.name()

    def accept(self, info: TrackInfo | None = None) -> TrackProducer:
        """Accept the request as a raw track.

        ``info`` fixes the track's timescale, priority, and cache; omit for defaults.
        """
        return TrackProducer(self._inner.accept(_track_info(info)))

    def demand(self) -> TrackDemand:
        """Watch this track's subscribers, including after accepting it."""
        return TrackDemand(self._inner.demand())

    def requests(self) -> TrackRequests:
        """Create a fetch handler before accepting this requested track."""
        return TrackRequests(self._inner.requests())

    def abort(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.abort(error_code)


class Fill:
    """A run of groups readers miss, for a :class:`TrackRequests` to serve.

    Call :meth:`accept` for each group you have and :meth:`drop_groups` for the rest;
    :meth:`next` names what is still owed, in the fill's order. Dropping the fill names
    whatever is left as dropped while a reader still wants it; once nobody does, it is
    withdrawn, so stop serving it when :meth:`unused` returns. Stop serving a single group
    when :meth:`unwanted` returns for it, and move on to :meth:`next`.
    """

    def __init__(self, inner: MoqFill) -> None:
        self._inner = inner

    @property
    def start_group(self) -> int:
        """The first group to fill."""
        return self._inner.start_group()

    @property
    def start_frame(self) -> int:
        """The frame within the first group to start from."""
        return self._inner.start_frame()

    @property
    def end_group(self) -> int:
        """The exclusive end group."""
        return self._inner.end_group()

    @property
    def priority(self) -> int:
        """The most urgent delivery priority among the readers waiting on it."""
        return self._inner.priority()

    @property
    def max_delay(self) -> timedelta | None:
        """The most tolerant age budget among its readers, ``None`` for the publisher's max age alone."""
        return self._inner.max_delay()

    @property
    def order(self) -> MoqOrder:
        """Which end to fill first."""
        return self._inner.order()

    def next(self) -> int | None:
        """The next group still owed in the fill's order, ``None`` once every group is answered."""
        return self._inner.next()

    def accept(self, sequence: int) -> GroupProducer:
        """Insert group ``sequence`` and return a producer for it.

        The first group starts at :attr:`start_frame`, so its frames keep their indices.
        """
        return GroupProducer(self._inner.accept(sequence))

    def drop_groups(self, start: int, end: int) -> None:
        """Name groups ``start`` up to the exclusive ``end`` as never to be delivered."""
        self._inner.drop_groups(start, end)

    def is_used(self) -> bool:
        """Whether a reader still waits on this fill."""
        return self._inner.is_used()

    async def unused(self) -> None:
        """Wait until no reader waits on this fill."""
        await self._inner.unused()

    def is_wanted(self, sequence: int) -> bool:
        """Whether a reader still waits for group ``sequence`` from this fill.

        False once every reader took it or narrowed past it, or nobody waits on the fill.
        """
        return self._inner.is_wanted(sequence)

    async def unwanted(self, sequence: int) -> None:
        """Wait until no reader waits for group ``sequence`` from this fill, so stop serving it."""
        await self._inner.unwanted(sequence)


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

    def cancel(self) -> None:
        """Stop accepting track requests and release the underlying handle."""
        self._inner.cancel()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface.

    It fills tracks; announcing belongs to the :class:`moq.Publication` that
    :meth:`moq.OriginProducer.create_broadcast` returns, the way to publish at a
    path. Constructing one directly creates a standalone broadcast for local pub/sub.
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

        Tracks still open on it end with ``Unroutable``, which readers take as the publisher
        leaving, and their later calls raise ``moq.Error.Closed``; finish a track first to end it
        cleanly. Closing again is a no-op.
        """
        self._inner.close()

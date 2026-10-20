"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

from datetime import timedelta
from typing import TYPE_CHECKING

from moq_ffi import (
    MoqBroadcastDynamic,
    MoqBroadcastProducer,
    MoqGroupDemand,
    MoqGroupProducer,
    MoqGroupRequest,
    MoqTrackDemand,
    MoqTrackDynamic,
    MoqTrackProducer,
    MoqTrackRequest,
)

from ._records import _subscription, _track_info
from ._stream import _Stream
from .types import (
    Frame,
    Route,
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

    def dynamic(self) -> TrackDynamic:
        """Serve fetches for groups that are not currently cached."""
        return TrackDynamic(self._inner.dynamic())

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

    def dynamic(self) -> TrackDynamic:
        """Create a fetch handler before accepting this requested track."""
        return TrackDynamic(self._inner.dynamic())

    def abort(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.abort(error_code)


class GroupDemand:
    """A watch-only handle to the callers waiting on a requested group.

    Returned by ``GroupRequest.demand()``. Weak: holding it does not keep the
    request alive. The last caller to leave withdraws the request, so once
    unused, demand never returns: drop the request. Waits raise
    ``moq.Error.Closed`` after the request is accepted or dropped, and keep
    the rejection reason after it is rejected.
    """

    def __init__(self, inner: MoqGroupDemand) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence of the group this watches."""
        return self._inner.sequence()

    def is_used(self) -> bool:
        """Whether the group has at least one waiting caller right now."""
        return self._inner.is_used()

    async def used(self) -> None:
        """Wait until the group has at least one waiting caller."""
        await self._inner.used()

    async def unused(self) -> None:
        """Wait until the group has no waiting callers."""
        await self._inner.unused()


class GroupRequest:
    """A request to produce one uncached group for a fetch consumer."""

    def __init__(self, inner: MoqGroupRequest) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The requested group sequence within the track."""
        return self._inner.sequence()

    @property
    def priority(self) -> int:
        """The consumer's delivery priority for this fetch."""
        return self._inner.priority()

    def demand(self) -> GroupDemand:
        """A watch-only handle to whether any caller still wants this group."""
        return GroupDemand(self._inner.demand())

    def accept(self) -> GroupProducer:
        """Accept the request and return a producer for the group."""
        return GroupProducer(self._inner.accept())

    def abort(self, error_code: int) -> None:
        """Reject the fetch with an application error code."""
        self._inner.abort(error_code)


class TrackDynamic(_Stream[GroupRequest]):
    """Async source of uncached group requests for one track.

    Usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqTrackDynamic) -> None:
        self._inner = inner

    async def _next(self) -> GroupRequest:
        return await self.requested_group()

    async def requested_group(self) -> GroupRequest:
        """Wait for the next uncached group request."""
        return GroupRequest(await self._inner.requested_group())

    def cancel(self) -> None:
        """Cancel current and future group request waits."""
        self._inner.cancel()


class BroadcastDynamic(_Stream[TrackRequest]):
    """Async source of tracks requested by subscribers.

    Hold this object while subscriptions to unknown tracks should be accepted.
    Usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqBroadcastDynamic) -> None:
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

    Constructing one directly creates a standalone broadcast for serving dynamic
    requests (:meth:`moq.BroadcastRequest.accept`) or local pub/sub. To publish at
    a path, use :meth:`moq.OriginProducer.create_broadcast` instead.
    """

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    @classmethod
    def _from_inner(cls, inner: MoqBroadcastProducer) -> BroadcastProducer:
        """Wrap an existing FFI producer (e.g. one created from an origin)."""
        self = cls.__new__(cls)
        self._inner = inner
        return self

    def dynamic(self) -> BroadcastDynamic:
        """Accept subscriptions to tracks that are not published yet."""
        return BroadcastDynamic(self._inner.dynamic())

    def announce(self, route: Route | None = None) -> None:
        """Advertise this broadcast's exact path as a route.

        Announcing again re-prices the route in place. Until announced, the
        broadcast is invisible and unroutable for local consumers and peers alike.
        """
        self._inner.announce(route if route is not None else Route())

    def unannounce(self) -> None:
        """Retract this broadcast's advertisement, if any, from local consumers and peers alike."""
        self._inner.unannounce()

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

        Tracks already subscribed carry on to their own end. Closing again is a no-op.
        """
        self._inner.close()

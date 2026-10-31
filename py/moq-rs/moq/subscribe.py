"""Consumer wrappers for broadcasts, catalogs, and media tracks."""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import TYPE_CHECKING

from moq_ffi import (
    MoqBroadcastConsumer,
    MoqFrame,
    MoqGroupConsumer,
    MoqTrackConsumer,
)

from ._records import _subscription
from ._stream import _Stream
from .types import (
    Datagram,
    FetchGroupOptions,
    Frame,
    Subscription,
    TrackInfo,
)

if TYPE_CHECKING:
    from .media import Audio, Video


def _frame(frame: MoqFrame | None) -> Frame | None:
    return None if frame is None else Frame._from_ffi(frame)


class GroupConsumer(_Stream[Frame]):
    """Async iterator of raw frames within a single group."""

    def __init__(self, inner: MoqGroupConsumer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    async def _next(self) -> Frame | None:
        return await self.read_frame()

    async def read_frame(self) -> Frame | None:
        """Read the next raw frame. Returns `None` when the group ends."""
        return _frame(await self._inner.read_frame())

    def cancel(self) -> None:
        """Cancel reading this group and stop delivering frames."""
        self._inner.cancel()


class TrackConsumer(_Stream[GroupConsumer]):
    """Async iterator of groups from a track, in sequence order.

    Iterating yields groups via :meth:`next_group`. Use :meth:`recv_group` (or
    :meth:`groups_as_arrived`) for arrival order instead.

    Each group is itself an async iterator of raw frames. Same pattern as
    moq-boy's status/command tracks (one frame per group), but multi-frame
    groups are also supported.
    """

    def __init__(self, inner: MoqTrackConsumer) -> None:
        self._inner = inner

    async def _next(self) -> GroupConsumer | None:
        return await self.next_group()

    async def groups_as_arrived(self) -> AsyncIterator[GroupConsumer]:
        """Iterate groups in arrival order, including out-of-sequence deliveries.

        The default iteration uses sequence order instead. Use this for live
        consumption where latency matters more than order.
        """
        while True:
            group = await self.recv_group()
            if group is None:
                return
            yield group

    async def recv_group(self) -> GroupConsumer | None:
        """Return the next group in arrival order. Returns `None` when the track ends.

        Groups are returned as they arrive on the wire, which may be out of sequence
        order. Use this for live consumption where latency matters more than order.
        """
        group = await self._inner.recv_group()
        if group is None:
            return None
        return GroupConsumer(group)

    async def next_group(self) -> GroupConsumer | None:
        """Return the next group in sequence order, skipping forward if behind.

        Returns `None` when the track ends. Shares the sequence cursor with
        :meth:`read_frame`: a group one method has already taken is not
        returned by the other. This is what the default iteration yields; use
        `recv_group` when latency matters more than order.
        """
        group = await self._inner.next_group()
        if group is None:
            return None
        return GroupConsumer(group)

    async def read_frame(self) -> Frame | None:
        """Read the first raw frame of the next group.

        Convenience for tracks using one-frame-per-group (like moq-boy's
        status/command tracks). Completed empty groups are skipped. Returns
        `None` only when the track ends. Cancelling one call keeps the current
        group so a later :meth:`read_frame` or :meth:`next_group` still sees it.
        """
        return _frame(await self._inner.read_frame())

    async def recv_datagram(self) -> Datagram | None:
        """Receive the next best-effort datagram in arrival order.

        Returns ``None`` when the track ends. Datagrams are unavailable over stream-only
        transports and older wire versions.
        """
        datagram = await self._inner.recv_datagram()
        return None if datagram is None else Datagram._from_ffi(datagram)

    def info(self) -> TrackInfo:
        """Return the publisher-side track properties."""
        return TrackInfo._from_ffi(self._inner.info())

    def update(self, subscription: Subscription) -> None:
        """Change this subscriber's delivery preferences.

        Raises on a time anchor the track can't serve.
        """
        self._inner.update(subscription._ffi())

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering groups."""
        self._inner.cancel()


class BroadcastConsumer:
    """The consume side of one broadcast: subscribe to its tracks, catalog, and media.

    Built by resolving a request. Use the ``subscribe_*`` / :meth:`fetch_group`
    methods to pull tracks and media.
    """

    def __init__(self, inner: MoqBroadcastConsumer) -> None:
        self._inner = inner

    async def closed(self) -> None:
        """Wait until the broadcast ends: its publisher closed it, or every producer left."""
        await self._inner.closed()

    async def subscribe_track(self, name: str, subscription: Subscription | None = None) -> TrackConsumer:
        """Subscribe to a track and receive arbitrary byte payloads.

        ``subscription`` tunes delivery priority, group range, and staleness; omit for defaults.
        """
        return TrackConsumer(await self._inner.subscribe_track(name, _subscription(subscription)))

    async def fetch_group(
        self,
        name: str,
        sequence: int,
        options: FetchGroupOptions | None = None,
    ) -> GroupConsumer:
        """Fetch one complete group by track name and group sequence.

        This does not hold a live subscription. The returned group may still be
        receiving frames, so iterate it until completion.
        """
        return GroupConsumer(await self._inner.fetch_group(name, sequence, options))

    async def resolve(self, id: str, rendition: Video | Audio) -> "BroadcastConsumer":
        """Resolve the catalog rendition ``id`` to the broadcast serving its track.

        The rendition's ``broadcast`` reference, ``None`` or empty, names this
        broadcast; anything else names a sibling relative to it (e.g. ``./source``).
        Its ``epoch`` is the publisher instance the reference names: a broadcast
        proven to serve another instance raises instead of resolving, and a route
        without an epoch cannot prove one, so there it is best effort. Call it on a
        rendition that carries a reference before :meth:`subscribe_track` or
        :meth:`fetch_group`, which take a track name rather than a rendition: the
        rendition's ``track``, or ``id`` when unset. The rendition-based consumers in
        the media, audio, and video namespaces resolve it themselves.

        Raises if this broadcast came from a local producer rather than an origin,
        since a standalone broadcast has no sibling to name.
        """
        from .media import _rendition

        return BroadcastConsumer(await self._inner.resolve(_rendition(id, rendition)))

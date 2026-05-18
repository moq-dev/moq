"""Consumer wrappers — subscribe to broadcasts, catalogs, and media tracks."""

from __future__ import annotations

from moq_ffi import (
    Container,
    MoqBroadcastConsumer,
    MoqCatalogConsumer,
    MoqGroupConsumer,
    MoqMediaConsumer,
    MoqTrackConsumer,
)

from .types import Catalog, Frame


class MediaConsumer:
    """Wraps MoqMediaConsumer as an async iterator of Frame."""

    def __init__(self, inner: MoqMediaConsumer) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> Frame:
        frame = await self._inner.next()
        if frame is None:
            raise StopAsyncIteration
        return frame

    def cancel(self) -> None:
        self._inner.cancel()


class GroupConsumer:
    """Async iterator of byte payloads within a single group."""

    def __init__(self, inner: MoqGroupConsumer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    def __aiter__(self):
        return self

    async def __anext__(self) -> bytes:
        frame = await self._inner.read_frame()
        if frame is None:
            raise StopAsyncIteration
        return frame

    def cancel(self) -> None:
        self._inner.cancel()


class TrackConsumer:
    """Async iterator of groups from a track.

    Each group is itself an async iterator of byte payloads. Same pattern as
    moq-boy's status/command tracks (one frame per group), but multi-frame
    groups are also supported.
    """

    def __init__(self, inner: MoqTrackConsumer) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> GroupConsumer:
        group = await self.recv_group()
        if group is None:
            raise StopAsyncIteration
        return group

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

        Returns `None` when the track ends. Use this when order matters more than
        latency; `recv_group` is preferred for live consumption.
        """
        group = await self._inner.next_group()
        if group is None:
            return None
        return GroupConsumer(group)

    async def read_frame(self) -> bytes | None:
        """Read the first frame of the next group.

        Convenience for tracks using one-frame-per-group (like moq-boy's
        status/command tracks). Returns `None` when the track ends.
        """
        return await self._inner.read_frame()

    def cancel(self) -> None:
        self._inner.cancel()


class CatalogConsumer:
    """Wraps MoqCatalogConsumer as an async iterator of Catalog."""

    def __init__(self, inner: MoqCatalogConsumer) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> Catalog:
        catalog = await self._inner.next()
        if catalog is None:
            raise StopAsyncIteration
        return catalog

    def cancel(self) -> None:
        self._inner.cancel()


class BroadcastConsumer:
    """Wraps MoqBroadcastConsumer with convenience methods."""

    def __init__(self, inner: MoqBroadcastConsumer) -> None:
        self._inner = inner

    def subscribe_catalog(self) -> CatalogConsumer:
        return CatalogConsumer(self._inner.subscribe_catalog())

    def subscribe_track(self, name: str) -> TrackConsumer:
        """Subscribe to a track — receive arbitrary byte payloads."""
        return TrackConsumer(self._inner.subscribe_track(name))

    def subscribe_media(self, name: str, container: Container, max_latency_ms: int) -> MediaConsumer:
        return MediaConsumer(self._inner.subscribe_media(name, container, max_latency_ms))

    async def catalog(self) -> Catalog:
        """Convenience: subscribe and return the first catalog."""
        consumer = self.subscribe_catalog()
        return await anext(consumer)

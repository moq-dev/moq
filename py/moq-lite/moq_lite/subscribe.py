"""Consumer wrappers — subscribe to broadcasts, catalogs, and media tracks."""

from __future__ import annotations

from moq_ffi import (
    Container,
    MoqBroadcastConsumer,
    MoqCatalogConsumer,
    MoqMediaConsumer,
    MoqRawConsumer,
    MoqRawGroupConsumer,
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


class RawGroupConsumer:
    """Async iterator of byte payloads within a single group."""

    def __init__(self, inner: MoqRawGroupConsumer) -> None:
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


class RawConsumer:
    """Async iterator of groups from a raw track.

    Each group is itself an async iterator of byte payloads. Same pattern as
    moq-boy's status/command tracks (one frame per group), but multi-frame
    groups are also supported.
    """

    def __init__(self, inner: MoqRawConsumer) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> RawGroupConsumer:
        group = await self._inner.next_group()
        if group is None:
            raise StopAsyncIteration
        return RawGroupConsumer(group)

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

    def subscribe_raw(self, name: str) -> RawConsumer:
        """Subscribe to a raw track — receive arbitrary byte payloads."""
        return RawConsumer(self._inner.subscribe_raw(name))

    def subscribe_media(self, name: str, container: Container, max_latency_ms: int) -> MediaConsumer:
        return MediaConsumer(self._inner.subscribe_media(name, container, max_latency_ms))

    async def catalog(self) -> Catalog:
        """Convenience: subscribe and return the first catalog."""
        consumer = self.subscribe_catalog()
        return await anext(consumer)

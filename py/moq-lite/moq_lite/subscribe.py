"""Consumer wrappers — subscribe to broadcasts, catalogs, and media tracks."""

from __future__ import annotations

from moq_ffi import MoqBroadcastConsumer, MoqCatalogConsumer, MoqMediaConsumer

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

    def subscribe_media(self, name: str, max_latency_ms: int = 10_000) -> MediaConsumer:
        return MediaConsumer(self._inner.subscribe_media(name, max_latency_ms))

    async def catalog(self) -> Catalog:
        """Convenience: subscribe and return the first catalog."""
        consumer = self.subscribe_catalog()
        return await anext(consumer)

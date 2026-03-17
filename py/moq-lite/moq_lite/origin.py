"""Origin wrappers — manage announcements and broadcast discovery."""

from __future__ import annotations

from moq_ffi import MoqAnnounced, MoqAnnouncedBroadcast, MoqAnnouncement, MoqOriginConsumer, MoqOriginProducer

from .publish import BroadcastProducer
from .subscribe import BroadcastConsumer


class Announcement:
    """Wraps MoqAnnouncement — a discovered broadcast."""

    def __init__(self, inner: MoqAnnouncement) -> None:
        self._inner = inner

    @property
    def path(self) -> str:
        return self._inner.path()

    @property
    def broadcast(self) -> BroadcastConsumer:
        return BroadcastConsumer(self._inner.broadcast())


class Announced:
    """Wraps MoqAnnounced as an async iterator of Announcement."""

    def __init__(self, inner: MoqAnnounced) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Announcement:
        result = await self._inner.next()
        if result is None:
            raise StopAsyncIteration
        return Announcement(result)

    def cancel(self) -> None:
        self._inner.cancel()


class AnnouncedBroadcast:
    """Wraps MoqAnnouncedBroadcast — awaitable for a specific broadcast."""

    def __init__(self, inner: MoqAnnouncedBroadcast) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    async def available(self) -> BroadcastConsumer:
        return BroadcastConsumer(await self._inner.available())

    def __await__(self):
        return self.available().__await__()

    def cancel(self) -> None:
        self._inner.cancel()


class OriginConsumer:
    """Wraps MoqOriginConsumer for discovering broadcasts."""

    def __init__(self, inner: MoqOriginConsumer) -> None:
        self._inner = inner

    def announced(self, prefix: str = "") -> Announced:
        return Announced(self._inner.announced(prefix))

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        return AnnouncedBroadcast(self._inner.announced_broadcast(path))


class OriginProducer:
    """Wraps MoqOriginProducer for publishing broadcasts."""

    def __init__(self) -> None:
        self._inner = MoqOriginProducer()

    def consume(self) -> OriginConsumer:
        return OriginConsumer(self._inner.consume())

    def publish(self, path: str, broadcast: BroadcastProducer) -> None:
        self._inner.publish(path, broadcast._inner)

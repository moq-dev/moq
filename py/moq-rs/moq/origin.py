"""Origin wrappers for announcements and broadcast discovery."""

from __future__ import annotations

from moq_ffi import (
    MoqAnnounced,
    MoqAnnouncedBroadcast,
    MoqAnnouncement,
    MoqBroadcastRequest,
    MoqOriginConsumer,
    MoqOriginDynamic,
    MoqOriginOptions,
    MoqOriginProducer,
)

from .publish import BroadcastProducer
from .subscribe import BroadcastConsumer


class Announcement:
    """Wraps MoqAnnouncement, a discovered broadcast."""

    def __init__(self, inner: MoqAnnouncement) -> None:
        self._inner = inner

    @property
    def path(self) -> str:
        return self._inner.path()

    @property
    def hops(self) -> list[int]:
        """Origin ids of the relay hops this broadcast traversed, oldest first."""
        return self._inner.hops()

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
    """Wraps MoqAnnouncedBroadcast, awaitable for a specific broadcast."""

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


class BroadcastRequest:
    """A requested broadcast that has not been accepted yet."""

    def __init__(self, inner: MoqBroadcastRequest) -> None:
        self._inner = inner

    @property
    def path(self) -> str:
        """The requested broadcast path."""
        return self._inner.path()

    def accept(self, broadcast: BroadcastProducer) -> None:
        """Serve the request with an unannounced broadcast."""
        self._inner.accept(broadcast._inner)

    def reject(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.reject(error_code)


class OriginDynamic:
    """Async source of broadcasts requested by consumers."""

    def __init__(self, inner: MoqOriginDynamic) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> BroadcastRequest:
        return await self.requested_broadcast()

    async def requested_broadcast(self) -> BroadcastRequest:
        return BroadcastRequest(await self._inner.requested_broadcast())

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

    async def request_broadcast(self, path: str) -> BroadcastConsumer:
        """Request a broadcast by path, resolving as soon as it can be served.

        Returns the announced broadcast immediately if one exists, otherwise falls
        back to a dynamic handler on the origin (if any), or raises if neither can
        serve it. Unlike `announced_broadcast`, this does not wait indefinitely for a
        future announcement.
        """
        return BroadcastConsumer(await self._inner.request_broadcast(path))


class OriginProducer:
    """Wraps MoqOriginProducer for publishing broadcasts."""

    def __init__(self, *, cache_capacity_bytes: int | None = None) -> None:
        self._inner = MoqOriginProducer(
            MoqOriginOptions(cache_capacity_bytes=cache_capacity_bytes)
        )

    @classmethod
    def _from_inner(cls, inner: MoqOriginProducer) -> OriginProducer:
        """Wrap an existing FFI producer (e.g. the one a `Session` owns)."""
        self = cls.__new__(cls)
        self._inner = inner
        return self

    def consume(self) -> OriginConsumer:
        return OriginConsumer(self._inner.consume())

    def dynamic(self) -> OriginDynamic:
        """Serve broadcasts that consumers request without an announcement."""
        return OriginDynamic(self._inner.dynamic())

    def announce(self, path: str, broadcast: BroadcastProducer) -> None:
        """Advertise ``broadcast`` at ``path`` so subscribers can discover it."""
        self._inner.announce(path, broadcast._inner)

    def publish(self, path: str, broadcast: BroadcastProducer) -> None:
        # Deprecated alias for announce(); kept for back-compat.
        self.announce(path, broadcast)

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
    """A single broadcast discovered via :meth:`OriginConsumer.announced`.

    Read its :attr:`path` and get a :attr:`broadcast` consumer to subscribe to it.
    """

    def __init__(self, inner: MoqAnnouncement) -> None:
        self._inner = inner
        self._broadcast: BroadcastConsumer | None = None

    @property
    def path(self) -> str:
        """The path this broadcast is announced at."""
        return self._inner.path()

    @property
    def broadcast(self) -> BroadcastConsumer:
        """The broadcast's consumer, one shared instance per announcement.

        Cached so stateful accessors (like the ``route_changed`` cursor)
        survive repeated property access.
        """
        if self._broadcast is None:
            self._broadcast = BroadcastConsumer(self._inner.broadcast())
        return self._broadcast


class Announced:
    """Async-iterable stream of :class:`Announcement` broadcasts as they appear.

    Usable as an async context manager; iterate with ``async for`` and it keeps
    yielding new broadcasts until cancelled.
    """

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
        """Stop iterating and release the underlying announcement stream."""
        self._inner.cancel()


class AnnouncedBroadcast:
    """Awaitable that resolves when the broadcast at a specific path is announced.

    ``await`` it (or call :meth:`available`) to get the :class:`BroadcastConsumer`
    once the broadcast becomes available. Usable as an async context manager.
    """

    def __init__(self, inner: MoqAnnouncedBroadcast) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    async def available(self) -> BroadcastConsumer:
        """Await the broadcast becoming available and return its consumer."""
        return BroadcastConsumer(await self._inner.available())

    def __await__(self):
        return self.available().__await__()

    def cancel(self) -> None:
        """Stop waiting for the broadcast and release the underlying handle."""
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

    def abort(self, error_code: int) -> None:
        """Abort the request with an application error code."""
        self._inner.abort(error_code)


class OriginDynamic:
    """Async source of broadcasts requested by consumers."""

    def __init__(self, inner: MoqOriginDynamic) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> BroadcastRequest:
        return await self.requested_broadcast()

    async def requested_broadcast(self) -> BroadcastRequest:
        """Await the next broadcast a consumer requested but that isn't published yet."""
        return BroadcastRequest(await self._inner.requested_broadcast())

    def cancel(self) -> None:
        """Stop serving dynamic requests and release the underlying handle."""
        self._inner.cancel()


class OriginConsumer:
    """The discovery side of an origin: find and subscribe to broadcasts.

    Iterate :meth:`announced` to watch broadcasts appear, await
    :meth:`announced_broadcast` for a specific path, or :meth:`request_broadcast`
    to resolve one as soon as it can be served.
    """

    def __init__(self, inner: MoqOriginConsumer) -> None:
        self._inner = inner

    def announced(self, prefix: str = "") -> Announced:
        """Async-iterate broadcasts announced under ``prefix`` (empty matches all)."""
        return Announced(self._inner.announced(prefix))

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        """Await announcement of the broadcast at exactly ``path``."""
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
    """The publishing side of an origin: announce broadcasts for consumers to discover.

    Call :meth:`create_broadcast` to publish at a path, :meth:`consume` for a
    matching :class:`OriginConsumer`, or :meth:`dynamic` to serve on-demand requests.
    """

    def __init__(self, *, cache_capacity_bytes: int | None = None) -> None:
        self._inner = MoqOriginProducer(MoqOriginOptions(cache_capacity_bytes=cache_capacity_bytes))

    @classmethod
    def _from_inner(cls, inner: MoqOriginProducer) -> OriginProducer:
        """Wrap an existing FFI producer (e.g. the one a `Session` owns)."""
        self = cls.__new__(cls)
        self._inner = inner
        return self

    def consume(self) -> OriginConsumer:
        """Create a consumer that discovers the broadcasts this origin publishes."""
        return OriginConsumer(self._inner.consume())

    def dynamic(self) -> OriginDynamic:
        """Serve broadcasts that consumers request without an announcement."""
        return OriginDynamic(self._inner.dynamic())

    def create_broadcast(self, path: str) -> BroadcastProducer:
        """Create a broadcast at ``path``, returning the producer that feeds it.

        The broadcast starts live: the origin announces the path so subscribers can
        discover it, becoming visible shortly after this returns. Toggle
        discoverability with :meth:`BroadcastProducer.set_announce`; ``finish()``
        unpublishes immediately, while dropping the producer without finishing
        lingers briefly so a replacement publisher can take over.
        """
        return BroadcastProducer._from_inner(self._inner.create_broadcast(path))

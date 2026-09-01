"""Origin wrappers for announcements and broadcast discovery."""

from __future__ import annotations

from moq_ffi import (
    MoqAnnounce,
    MoqAnnounced,
    MoqAnnouncedBroadcast,
    MoqAnnouncement,
    MoqBroadcastRequest,
    MoqOriginConsumer,
    MoqOriginDynamic,
    MoqOriginOptions,
    MoqOriginProducer,
)
from moq_ffi import (
    MoqRoute as Route,
)

from .publish import BroadcastProducer
from .subscribe import BroadcastConsumer


class Announcement:
    """A route announcement (or retraction) from :meth:`OriginConsumer.announced`.

    A route claims that paths under :attr:`path` can be served; it carries no
    broadcast. Resolve a specific path with :meth:`OriginConsumer.request_broadcast`.
    By convention a publisher announces each broadcast's exact path, so
    subscribers can enumerate broadcasts from routes.
    """

    def __init__(self, inner: MoqAnnouncement) -> None:
        self._inner = inner

    @property
    def path(self) -> str:
        """The announced route's prefix, relative to the ``announced`` prefix."""
        return self._inner.path()

    @property
    def active(self) -> bool:
        """Whether the route is active (``True``) or was retracted (``False``).

        A repeated active announcement for the same path is a metadata update.
        """
        return self._inner.active()

    @property
    def route(self) -> Route:
        """The route serving the prefix: its relay hops and cost."""
        return self._inner.route()


class Announce:
    """A live route advertisement, from :meth:`OriginProducer.announce`.

    The route stays advertised until :meth:`cancel` (or garbage collection).
    Usable as a context manager, retracting on exit.
    """

    def __init__(self, inner: MoqAnnounce) -> None:
        self._inner = inner

    def __enter__(self):
        return self

    def __exit__(self, *exc) -> None:
        self.cancel()

    def update(self, route: Route) -> None:
        """Re-price the route in place: replace its hops and cost.

        The prefix is fixed at announce time; announce again to move it.
        """
        self._inner.update(route)

    def cancel(self) -> None:
        """Retract the route."""
        self._inner.cancel()


class Announced:
    """Async-iterable stream of :class:`Announcement` route updates as they arrive.

    Usable as an async context manager; iterate with ``async for`` and it keeps
    yielding announcements and retractions until cancelled.
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
    """Awaitable that resolves once a route covers a specific path.

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
        """Await a covering route and return the resolved broadcast consumer."""
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
        """Async-iterate route announcements under ``prefix`` (empty matches all)."""
        return Announced(self._inner.announced(prefix))

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        """Await a route covering ``path``, then resolve the broadcast there."""
        return AnnouncedBroadcast(self._inner.announced_broadcast(path))

    async def request_broadcast(self, path: str) -> BroadcastConsumer:
        """Request a broadcast by path, resolving as soon as it can be served.

        Resolution order: a local broadcast at the exact path, then the best
        announced route covering the path (served on demand by the session that
        announced it), then a dynamic handler on the origin (if any); raises if
        nothing can serve it. Unlike `announced_broadcast`, this does not wait
        for a future announcement.
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

        The broadcast starts announced: the origin advertises the exact path as a
        route so subscribers can discover it, becoming visible shortly after this
        returns. Toggle discoverability with :meth:`BroadcastProducer.set_announce`;
        ``finish()`` unpublishes immediately, while dropping the producer without
        finishing also unpublishes but reads to subscribers as a failure rather
        than a deliberate end.
        """
        return BroadcastProducer._from_inner(self._inner.create_broadcast(path))

    def announce(self, prefix: str, route: Route | None = None) -> Announce:
        """Advertise a route: a claim that paths under ``prefix`` can be served.

        Hold the returned :class:`Announce` for as long as the route should stay
        advertised; ``route`` carries the optional metadata (relay hops and cost).
        Announcing is independent of :meth:`create_broadcast`: announce one short
        prefix and serve requests beneath it with :meth:`dynamic`.
        """
        return Announce(self._inner.announce(prefix, route if route is not None else Route()))

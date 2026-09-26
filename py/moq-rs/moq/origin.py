"""Origin wrappers for announcements and broadcast discovery."""

from __future__ import annotations

from moq_ffi import (
    MoqAnnounce,
    MoqAnnounceConfig,
    MoqAnnounceConsumer,
    MoqAnnouncedBroadcast,
    MoqAnnounceEvent,
    MoqBroadcastRequest,
    MoqOriginConfig,
    MoqOriginConsumer,
    MoqOriginDynamic,
    MoqOriginProducer,
)
from moq_ffi import (
    MoqRoute as Route,
)

from .publish import BroadcastProducer
from .subscribe import BroadcastConsumer

Announce = MoqAnnounce
"""A route over a prefix: the origin-relative ``prefix``, what each filter wildcard
matched (``captures``, ``None`` for a partial overlap), and the ``route`` serving it.

It carries no broadcast. Resolve a specific path with
:meth:`OriginConsumer.request_broadcast`. By convention a publisher announces
each broadcast's exact path, so subscribers can enumerate broadcasts from routes.
"""

AnnounceEvent = MoqAnnounceEvent
"""What :class:`AnnounceConsumer` yields: ``AnnounceEvent.ANNOUNCED``,
``AnnounceEvent.UPDATED``, or ``AnnounceEvent.RETRACTED`` carrying an
:data:`Announce` as ``announce``, or ``AnnounceEvent.LIVE`` once every route
live at subscribe time has been delivered.
"""


class AnnounceConsumer:
    """Async-iterable stream of :data:`AnnounceEvent` as they arrive.

    Usable as an async context manager; iterate with ``async for`` and it keeps
    yielding events until cancelled. Break on ``AnnounceEvent.LIVE`` to list
    what is live and stop.
    """

    def __init__(self, inner: MoqAnnounceConsumer) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> MoqAnnounceEvent:
        result = await self._inner.next()
        if result is None:
            raise StopAsyncIteration
        return result

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

    def reject(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.reject(error_code)


class OriginDynamic:
    """A served route: advertises a prefix and yields requests beneath it.

    Usable as an async context manager that cancels the route on exit.
    """

    def __init__(self, inner: MoqOriginDynamic) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> BroadcastRequest:
        return await self.requested_broadcast()

    async def requested_broadcast(self) -> BroadcastRequest:
        """Await the next broadcast a consumer requested but that isn't published yet."""
        return BroadcastRequest(await self._inner.requested_broadcast())

    def update(self, route: Route) -> None:
        """Re-price the route in place: replace its hops and costs."""
        self._inner.update(route)

    def cancel(self) -> None:
        """Stop serving and retract the route."""
        self._inner.cancel()


class OriginConsumer:
    """The discovery side of an origin: find and subscribe to broadcasts.

    Iterate :meth:`announced` to watch broadcasts appear, await
    :meth:`announced_broadcast` for a specific path, or :meth:`request_broadcast`
    to resolve one as soon as it can be served.
    """

    def __init__(self, inner: MoqOriginConsumer) -> None:
        self._inner = inner

    def announced(self, prefix: str = "", *, filter: str | None = None, hidden: bool = False) -> AnnounceConsumer:
        """Iterate routes in the literal ``prefix`` matching the optional pattern ``filter``.

        Paths with a segment starting with ``.`` below ``prefix`` are hidden unless ``hidden``.
        """
        return AnnounceConsumer(self._inner.announced(MoqAnnounceConfig(prefix=prefix, filter=filter, hidden=hidden)))

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        """Await the broadcast at ``path``, resolving once something can serve it.

        A broadcast created on this origin resolves once it is announced, like a
        remote one.
        """
        return AnnouncedBroadcast(self._inner.announced_broadcast(path))

    async def request_broadcast(self, path: str) -> BroadcastConsumer:
        """Request a broadcast by path, resolving as soon as it can be served.

        Resolves through the best announced route covering the path: an
        announced broadcast on this origin, a route a session announced (served
        on demand by that session), or a dynamic handler on the origin. An
        unannounced broadcast is unroutable. Unlike
        `announced_broadcast`, this answers for what is reachable now and raises
        if nothing can serve the path.
        """
        return BroadcastConsumer(await self._inner.request_broadcast(path))


class OriginProducer:
    """The publishing side of an origin: announce broadcasts for consumers to discover.

    Call :meth:`create_broadcast` to publish at a path, :meth:`consume` for a
    matching :class:`OriginConsumer`, or :meth:`dynamic` to advertise a prefix
    and serve on-demand requests. Create, :meth:`dynamic` if tracks are served
    on demand, populate, then :meth:`BroadcastProducer.announce`.
    """

    def __init__(self, *, cache_capacity_bytes: int | None = None) -> None:
        self._inner = MoqOriginProducer(MoqOriginConfig(cache_capacity_bytes=cache_capacity_bytes))

    @classmethod
    def _from_inner(cls, inner: MoqOriginProducer) -> OriginProducer:
        """Wrap an existing FFI producer (e.g. the one a `Session` owns)."""
        self = cls.__new__(cls)
        self._inner = inner
        return self

    def consume(self) -> OriginConsumer:
        """Create a consumer that discovers the broadcasts this origin publishes."""
        return OriginConsumer(self._inner.consume())

    def dynamic(self, prefix: str, route: Route | None = None) -> OriginDynamic:
        """Advertise ``prefix`` and serve the requests beneath it.

        A route claims ``prefix`` and every path beneath it (``""`` claims every
        path). A service that only serves some of them rejects the rest as they
        are requested. Hold the returned handle while the route should stay
        advertised.
        """
        return OriginDynamic(self._inner.dynamic(prefix, route if route is not None else Route()))

    def create_broadcast(self, path: str) -> BroadcastProducer:
        """Create a broadcast at ``path``, returning the producer that feeds it.

        The broadcast is invisible and unroutable, for this origin's consumers
        and peers alike, until :meth:`BroadcastProducer.announce`. Announce it
        after populating tracks. Create,
        :meth:`dynamic` if tracks are served on demand, populate, then announce.
        ``finish()`` unpublishes immediately, while dropping the producer without
        finishing also unpublishes but reads to subscribers as a failure rather
        than a deliberate end.
        """
        return BroadcastProducer._from_inner(self._inner.create_broadcast(path))

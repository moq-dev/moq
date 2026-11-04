"""Origin wrappers for announcements and broadcast discovery."""

from __future__ import annotations

from moq_ffi import (
    MoqAnnounce,
    MoqAnnounceConfig,
    MoqAnnounceConsumer,
    MoqAnnounceEvent,
    MoqBroadcastRequest,
    MoqFollowing,
    MoqOriginConfig,
    MoqOriginConsumer,
    MoqOriginProducer,
    MoqRoute,
    MoqRouteOptions,
)

from ._stream import _Stream
from .publish import BroadcastProducer
from .subscribe import BroadcastConsumer

Announce = MoqAnnounce
"""A route over a prefix: the origin-relative ``prefix``, what each filter wildcard
matched (the required list ``captures``), and the ``route`` serving it.
The list is empty when the filter has no wildcards.

It carries no broadcast. Resolve a specific path with
:meth:`OriginConsumer.request`. By convention a publisher announces
each broadcast's exact path, so subscribers can enumerate broadcasts from routes.
"""

AnnounceEvent = MoqAnnounceEvent
"""What :class:`AnnounceConsumer` yields: :data:`AnnounceEventStart`,
:data:`AnnounceEventUpdate`, :data:`AnnounceEventEnd`, :data:`AnnounceEventRestart`,
or :data:`AnnounceEventTakeover`, each carrying an :data:`Announce` as ``announce``.
"""

AnnounceEventStart = MoqAnnounceEvent.START
"""A route now covers the prefix; the stream had none there."""

AnnounceEventUpdate = MoqAnnounceEvent.UPDATE
"""The route covering the prefix changed hops or cost."""

AnnounceEventEnd = MoqAnnounceEvent.END
"""No route covers the prefix any more; carries its last route."""

AnnounceEventRestart = MoqAnnounceEvent.RESTART
"""Another publisher instance now serves the prefix: drop what was resolved under
it and request afresh."""

AnnounceEventTakeover = MoqAnnounceEvent.TAKEOVER
"""A route now covers the prefix and continues what the route at ``predecessor``
served beneath it: keep what was resolved through that one."""


class AnnounceConsumer(_Stream[MoqAnnounceEvent]):
    """Async-iterable stream of :data:`AnnounceEvent` as they arrive.

    Usable as an async context manager; iterate with ``async for`` and it keeps
    yielding events until cancelled.
    """

    def __init__(self, inner: MoqAnnounceConsumer) -> None:
        self._inner = inner

    async def _next(self) -> MoqAnnounceEvent | None:
        return await self._inner.next()

    def cancel(self) -> None:
        """Stop iterating and release the underlying announcement stream."""
        self._inner.cancel()


class Following:
    """Follows a specific path across the publisher instances that serve it.

    ``await`` it (or call :meth:`available`) to get the :class:`BroadcastConsumer`
    once the broadcast becomes available, and again for each instance that takes over.
    Usable as an async context manager.
    """

    def __init__(self, inner: MoqFollowing) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    async def available(self) -> BroadcastConsumer:
        """Await the next publisher instance serving the path and return its consumer.

        The first call resolves the instance serving the path once one is announced. Each
        later call waits for another to take over (a restarted publisher, or a replacement
        session) or for the broadcast to return after it went offline. Drop what was read
        from the previous instance then: its tracks end when its session does.
        """
        return BroadcastConsumer(await self._inner.available())

    def __await__(self):
        return self.available().__await__()

    def cancel(self) -> None:
        """Stop waiting for the broadcast and release the underlying handle."""
        self._inner.cancel()


def _options(*, hidden: bool, cost: int, instance: str | None, epoch: int) -> MoqRouteOptions:
    return MoqRouteOptions(hidden=hidden, cost=cost, instance=instance, epoch=epoch)


class BroadcastRequest:
    """A requested broadcast that has not been answered yet."""

    def __init__(self, inner: MoqBroadcastRequest) -> None:
        self._inner = inner

    @property
    def path(self) -> str:
        """The requested broadcast path."""
        return self._inner.path()

    def route(self, *, hidden: bool = False, cost: int = 0, instance: str | None = None, epoch: int = 0) -> Route:
        """Answer with the path's own exact :class:`Route`, owning a new broadcast.

        The route serves a fresh instance unless ``instance`` names one, so a job that
        restarts is a new stream. Created visible, it announces now and answers the waiting
        requesters; created ``hidden``, they wait until :meth:`Route.announce`, so the job can
        fill its catalog first, and cancelling the route before then refuses them. Ending the
        route withdraws only this path.
        """
        return Route(self._inner.route(_options(hidden=hidden, cost=cost, instance=instance, epoch=epoch)))

    def serve(self, broadcast: BroadcastConsumer) -> None:
        """Serve held content under the prefix route's continuous sequence space, without an exact route."""
        self._inner.serve(broadcast._inner)

    def reject(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.reject(error_code)


class Route(_Stream[BroadcastRequest]):
    """A route on an origin: an exact route owning one broadcast, or a prefix route
    yielding the requests beneath it.

    Hold it for as long as the route should stay advertised: dropping or cancelling it
    retracts it, as does closing an exact route's broadcast. A broadcast handle alone does
    not keep it announced. Iterate a prefix route with ``async for`` for its
    :class:`BroadcastRequest` values; usable as an async context manager that cancels the
    route on exit.
    """

    def __init__(self, inner: MoqRoute) -> None:
        self._inner = inner
        self._broadcast: BroadcastProducer | None = None

    @property
    def broadcast(self) -> BroadcastProducer:
        """The broadcast this exact route owns, to fill with tracks.

        Raises ``moq.Error.Internal`` on a prefix route, which owns none: answer its requests instead.
        """
        if self._broadcast is None:
            self._broadcast = BroadcastProducer._from_inner(self._inner.broadcast())
        return self._broadcast

    @property
    def instance(self) -> str:
        """The stream this route serves, minted at creation unless one was named, and kept
        across :meth:`withdraw` and :meth:`announce`."""
        return self._inner.instance_id()

    def announce(self) -> None:
        """Make the route visible, or keep it so.

        A route from :meth:`BroadcastRequest.route` also answers the requesters waiting on
        its path. Raises ``moq.Error.Closed`` once its broadcast closed.
        """
        self._inner.announce()

    def withdraw(self) -> None:
        """Hide the route from everyone, keeping its broadcast and instance.

        :meth:`announce` brings the same stream back; tracks already in flight carry on.
        """
        self._inner.withdraw()

    def set_cost(self, cost: int) -> None:
        """Re-price the route in place: lower wins among routes covering the same prefix.

        A hidden route keeps the price for when it next announces. Raises
        ``moq.Error.Closed`` once its broadcast closed.
        """
        self._inner.set_cost(cost)

    async def _next(self) -> BroadcastRequest | None:
        return await self.requested()

    async def requested(self) -> BroadcastRequest | None:
        """Await the next broadcast a consumer requests beneath this prefix route.

        Serve, route, or reject it; the requester stays pending until then. Returns
        ``None`` once the origin closes, and at once on an exact route.
        """
        request = await self._inner.requested()
        return None if request is None else BroadcastRequest(request)

    def cancel(self) -> None:
        """Retract the route and release it now. An exact route's broadcast lives on until
        :meth:`BroadcastProducer.close`."""
        self._inner.cancel()


class OriginConsumer:
    """The discovery side of an origin: find and subscribe to broadcasts.

    Iterate :meth:`announced` to watch broadcasts appear, :meth:`follow` a
    specific path across its publisher instances, or :meth:`request` one
    instance as soon as it can be served.
    """

    def __init__(self, inner: MoqOriginConsumer) -> None:
        self._inner = inner

    def announced(self, prefix: str = "", *, filter: str | None = None, hidden: bool = False) -> AnnounceConsumer:
        """Iterate routes in the literal ``prefix`` matching the optional pattern ``filter``.

        Paths with a segment starting with ``.`` below ``prefix`` are hidden unless ``hidden``.
        """
        return AnnounceConsumer(self._inner.announced(MoqAnnounceConfig(prefix=prefix, filter=filter, hidden=hidden)))

    def follow(self, path: str, *, instance: str | None = None) -> Following:
        """Follow the broadcast at ``path``, resolving each publisher instance that serves it.

        A broadcast created on this origin resolves once it is announced, like a
        remote one. Pass an ``instance`` to follow only that stream: another one
        taking over is never handed out, and the stream is handed out again if it
        returns after an end.
        """
        return Following(self._inner.follow(path, instance))

    async def request(self, path: str, *, instance: str | None = None) -> BroadcastConsumer:
        """Request the broadcast at ``path``, resolving one publisher instance as soon as it can be served.

        Resolves through the best announced route covering the path: an
        announced broadcast on this origin, a route a session announced (served
        on demand by that session), or a dynamic handler on the origin. An
        unannounced broadcast is unroutable. Unlike :meth:`follow`, this
        answers for what is reachable now and raises if nothing can serve the
        path. Pass an ``instance`` to refuse any other stream.
        """
        return BroadcastConsumer(await self._inner.request(path, instance))


class OriginProducer:
    """The publishing side of an origin: route broadcasts for consumers to discover.

    Call :meth:`route` to publish a broadcast at a path, :meth:`route_prefix` to
    serve on-demand requests beneath a prefix, or :meth:`consume` for a matching
    :class:`OriginConsumer`.
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

    def route(
        self, path: str, *, hidden: bool = False, cost: int = 0, instance: str | None = None, epoch: int = 0
    ) -> Route:
        """Create a :class:`Route` at exactly ``path``, owning a new broadcast to fill.

        Visible at once unless ``hidden``: a hidden route is invisible and unroutable, for
        this origin's consumers and peers alike, until :meth:`Route.announce`, so create it
        hidden to fill the catalog first. ``cost`` prices it: lower wins. The route serves a
        fresh instance unless ``instance`` names a stable one, whose runs raise ``epoch``.
        Hold the route: dropping or cancelling it retracts it, and
        :meth:`BroadcastProducer.close` ends both.
        """
        return Route(self._inner.route(path, _options(hidden=hidden, cost=cost, instance=instance, epoch=epoch)))

    def route_prefix(
        self, prefix: str, *, hidden: bool = False, cost: int = 0, instance: str | None = None, epoch: int = 0
    ) -> Route:
        """Create a :class:`Route` over ``prefix`` that yields the requests beneath it.

        A route claims ``prefix`` and every path beneath it (``""`` claims every
        path). A service that only serves some of them rejects the rest as they
        are requested. Hold the route while it should stay advertised.
        """
        return Route(
            self._inner.route_prefix(prefix, _options(hidden=hidden, cost=cost, instance=instance, epoch=epoch))
        )

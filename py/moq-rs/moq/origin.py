"""Origin wrappers for announcements and broadcast discovery."""

from __future__ import annotations

from moq_ffi import (
    MoqAnnounce,
    MoqAnnounceConfig,
    MoqAnnounceConsumer,
    MoqAnnounceEvent,
    MoqBroadcastPrepared,
    MoqBroadcastRequest,
    MoqFollowing,
    MoqOriginConfig,
    MoqOriginConsumer,
    MoqOriginProducer,
    MoqOriginRequests,
    MoqPublication,
)

from ._stream import _Stream
from .publish import BroadcastProducer
from .subscribe import BroadcastConsumer
from .types import AnnounceRoute

Announce = MoqAnnounce
"""A route over a prefix: the origin-relative ``prefix``, what each filter wildcard
matched (``captures``), and the ``route`` serving it.

It carries no broadcast. Resolve a specific path with
:meth:`OriginConsumer.request`. By convention a publisher announces
each broadcast's exact path, so subscribers can enumerate broadcasts from routes.
"""

AnnounceEvent = MoqAnnounceEvent
"""What :class:`AnnounceConsumer` yields: :data:`AnnounceEventStart`,
:data:`AnnounceEventUpdate`, :data:`AnnounceEventEnd`, or
:data:`AnnounceEventRestart`, each carrying an :data:`Announce` as ``announce``.
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


class BroadcastRequest:
    """A requested broadcast that has not been accepted yet."""

    def __init__(self, inner: MoqBroadcastRequest) -> None:
        self._inner = inner

    @property
    def path(self) -> str:
        """The requested broadcast path."""
        return self._inner.path()

    def prepare(self, epoch: str) -> BroadcastPrepared:
        """Start an answer under a new epoch, hidden until published."""
        return BroadcastPrepared(self._inner.prepare(epoch))

    def reject(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.reject(error_code)


class BroadcastPrepared:
    """A claim answer being filled before its exact route is announced."""

    def __init__(self, inner: MoqBroadcastPrepared) -> None:
        self._inner = inner
        self._broadcast = BroadcastProducer._from_inner(inner.broadcast())

    @property
    def broadcast(self) -> BroadcastProducer:
        """The unannounced broadcast to fill before publishing; only `publish` announces it."""
        return self._broadcast

    def publish(self) -> Publication:
        """Announce the filled broadcast and resolve waiting requesters."""
        return Publication(self._inner.publish(), self._broadcast)


class Publication:
    """A broadcast published on an origin: the broadcast to fill and its announcement.

    Dropping it does not retract the route; :meth:`BroadcastProducer.close` does.
    """

    def __init__(self, inner: MoqPublication, broadcast: BroadcastProducer) -> None:
        self._inner = inner
        self._broadcast = broadcast

    @property
    def broadcast(self) -> BroadcastProducer:
        """The broadcast to fill, without announcement control."""
        return self._broadcast

    def announce(self, route: AnnounceRoute | None = None) -> None:
        """Advertise the broadcast's exact path as a route, or re-price it in place.

        Until announced, the broadcast is invisible and unroutable for local
        consumers and peers alike. Raises ``moq.Error.Closed`` once the broadcast closed.
        """
        self._inner.announce(route if route is not None else AnnounceRoute())

    def unannounce(self) -> None:
        """Retract the advertisement, if any, keeping the filled tracks alive.

        Announcing again brings it back. Raises ``moq.Error.Closed`` once the broadcast closed.
        """
        self._inner.unannounce()


class OriginRequests(_Stream[BroadcastRequest]):
    """A served route: advertises a prefix and yields requests beneath it.

    Usable as an async context manager that cancels the route on exit.
    """

    def __init__(self, inner: MoqOriginRequests) -> None:
        self._inner = inner

    async def _next(self) -> BroadcastRequest:
        return await self.requested_broadcast()

    async def requested_broadcast(self) -> BroadcastRequest:
        """Await the next broadcast a consumer requested but that isn't published yet."""
        return BroadcastRequest(await self._inner.requested_broadcast())

    def cancel(self) -> None:
        """Stop serving and retract the route."""
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

    def follow(self, path: str, *, epoch: str | None = None) -> Following:
        """Follow the broadcast at ``path``, resolving each publisher instance that serves it.

        A broadcast created on this origin resolves once it is announced, like a
        remote one. Pass an ``epoch`` (UUIDv7 text) to follow only that instance.
        """
        return Following(self._inner.follow(path, epoch))

    async def request(self, path: str, *, epoch: str | None = None) -> BroadcastConsumer:
        """Request the broadcast at ``path``, resolving one publisher instance as soon as it can be served.

        Resolves through the best announced route covering the path: an
        announced broadcast on this origin, a route a session announced (served
        on demand by that session), or a dynamic handler on the origin. An
        unannounced broadcast is unroutable. Unlike :meth:`follow`, this
        answers for what is reachable now and raises if nothing can serve the
        path. Pass an ``epoch`` (UUIDv7 text) to refuse any other instance.
        """
        return BroadcastConsumer(await self._inner.request(path, epoch))


class OriginProducer:
    """The publishing side of an origin: announce broadcasts for consumers to discover.

    Call :meth:`create_broadcast` to publish at a path, :meth:`consume` for a
    matching :class:`OriginConsumer`, or :meth:`requests` to advertise a prefix
    and serve on-demand requests. Create, :meth:`requests` if tracks are served
    on demand, populate, then :meth:`Publication.announce`.
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

    def requests(self, prefix: str) -> OriginRequests:
        """Advertise ``prefix`` and serve the requests beneath it.

        A route claims ``prefix`` and every path beneath it (``""`` claims every
        path). A service that only serves some of them rejects the rest as they
        are requested. Hold the returned handle while the route should stay
        advertised.
        """
        return OriginRequests(self._inner.requests(prefix))

    def create_broadcast(self, path: str) -> Publication:
        """Create a broadcast at ``path``, returning its :class:`Publication`.

        Fill :attr:`Publication.broadcast`. The broadcast is invisible and
        unroutable, for this origin's consumers and peers alike, until
        :meth:`Publication.announce`. Announce it after populating tracks. Create,
        :meth:`requests` if tracks are served on demand, populate, then announce.
        :meth:`BroadcastProducer.close` ends it for good; dropping its last
        handle, the publication included, does the same.
        """
        inner = self._inner.create_broadcast(path)
        return Publication(inner, BroadcastProducer._from_inner(inner.broadcast()))

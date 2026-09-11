"""Session wrapper for an established MoQ connection."""

from __future__ import annotations

from moq_ffi import MoqBandwidth, MoqReservation, MoqSession

from .origin import OriginConsumer, OriginProducer
from .publish import TrackProducer
from .types import ConnectionStats, ConnectionStatus


class Session:
    """An established MoQ connection, returned by `Client` and `Request.accept()`.

    Hold the session to keep the connection alive; dropping it closes the
    connection. As an async context manager it shuts down gracefully on exit::

        session = await request.accept()
        async with session:
            await session.closed()
    """

    def __init__(self, inner: MoqSession) -> None:
        self._inner = inner

    async def __aenter__(self) -> Session:
        return self

    async def __aexit__(self, *exc) -> None:
        self.shutdown()

    async def closed(self) -> None:
        """Wait until the session is over.

        A client session resolves when its connection stops for good: it raises the
        terminal error when the connection gave up, and returns normally after a local
        :meth:`shutdown`/:meth:`cancel`. Transient drops the reconnect loop rides out
        do not resolve this; watch :meth:`status` for those. A server-accepted session
        raises the session's close reason."""
        await self._inner.closed()

    async def status(self) -> ConnectionStatus:
        """Wait for the connection status to differ from the one last reported.

        A client session reports ``CONNECTED`` first (the connect it was built from),
        then follows the reconnect loop: ``DISCONNECTED`` while redialing,
        ``CONNECTED`` again on success, ``MIGRATING`` during a GOAWAY handover. It
        raises once the connection stops for good. A server-accepted session's only
        transition is terminal: this waits for the close and raises its reason.

        This is the current status, not a queue of every edge: a drop that
        reconnects before you ask again is coalesced away, so the outages it hides
        are the ones that already healed. Do not count outages with it."""
        return await self._inner.status()

    def epoch(self) -> int:
        """The connection epoch: 1 for the connect that built this session, one more
        on each reconnect. A server-accepted session stays at 1.

        Pair it with :meth:`status` to log each reconnect by number; a
        ``CONNECTED`` status whose epoch grew is a reconnect."""
        return self._inner.epoch()

    def cancel(self, code: int) -> None:
        """Close the session with the given error code."""
        self._inner.cancel(code)

    def shutdown(self) -> None:
        """Graceful shutdown; equivalent to `cancel(0)` (0 means no error)."""
        self._inner.shutdown()

    def publisher(self) -> OriginProducer:
        """The publish-side origin: where local broadcasts are advertised to
        the remote. Either the origin wired before connect/accept, or one
        auto-created if none was set."""
        return OriginProducer._from_inner(self._inner.publisher())

    def consumer(self) -> OriginConsumer:
        """The subscribe-side origin: a read handle for announcements pushed by
        the remote."""
        return OriginConsumer(self._inner.consumer())

    def stats(self) -> ConnectionStats:
        """Snapshot the current connection statistics (RTT, bandwidth estimates,
        byte/packet counters). Cheap to call; intended for periodic polling.

        Individual fields are ``None`` when the transport backend doesn't report them."""
        return self._inner.stats()

    def bandwidth(self) -> Bandwidth:
        """The session's bandwidth allocator.

        Every call returns a handle to the same registry, so reservations made
        through one are visible to the others. A client handle survives
        reconnects: the grant is ``None`` while disconnected and resumes on the
        next connection.
        """
        return Bandwidth(self._inner.bandwidth())


class Bandwidth:
    """Divides one connection's send estimate among the tracks sharing it.

    Minted by :meth:`Session.bandwidth`. Clones share one reservation registry.
    """

    def __init__(self, inner: MoqBandwidth) -> None:
        self._inner = inner

    def reserve(self, track: TrackProducer, max_bps: int) -> Reservation:
        """Reserve up to ``max_bps`` for ``track``.

        ``max_bps`` is a ceiling, not a measurement: reserve the most the track
        can ever send. Drop the reservation to hand the room back.
        """
        return Reservation(self._inner.reserve(track._inner, max_bps))


class Reservation:
    """One track's standing claim on a :class:`Bandwidth`.

    :meth:`grant` is a snapshot: ``None`` means no estimate or no demand, so
    hold the current rate, and ``0`` is a real zero grant.
    """

    def __init__(self, inner: MoqReservation) -> None:
        self._inner = inner

    def grant(self) -> int | None:
        """This reservation's slice right now, in bits per second."""
        return self._inner.grant()

    def update(self, max_bps: int) -> None:
        """Change the ceiling, keeping the same claim."""
        self._inner.update(max_bps)

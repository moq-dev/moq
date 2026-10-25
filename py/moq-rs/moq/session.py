"""Session wrapper for a MoQ connection, including its initial native dial."""

from __future__ import annotations

from moq_ffi import MoqAllocator, MoqAuth, MoqBandwidthConsumer, MoqCredential, MoqReservation, MoqSession

from .origin import OriginConsumer, OriginProducer
from .publish import TrackDemand
from .types import ConnectionState, ConnectionStats, Grant


class Session:
    """A MoQ connection, returned by `Client` and `Request.accept()`.

    Hold the session to keep the connection alive; dropping it closes the
    connection. As an async context manager it drains on a clean exit and
    cancels on an error::

        session = await request.accept()
        async with session:
            await session.closed()
    """

    def __init__(self, inner: MoqSession) -> None:
        self._inner = inner

    async def __aenter__(self) -> Session:
        return self

    async def __aexit__(self, *exc) -> None:
        # A body error is the failure worth reporting. Draining behind it would wait
        # out the deadline and replace it with a delivery timeout.
        if exc[0] is None:
            self.shutdown()
            await self.closed()
        else:
            self.cancel(0)

    async def closed(self) -> None:
        """Wait until the session is over.

        A client session resolves when its connection stops for good: it raises the
        terminal error when the connection gave up, and returns normally after a local
        :meth:`cancel`. After :meth:`shutdown`, it waits for the drain and raises
        delivery failures. Transient drops the reconnect loop rides out
        do not resolve this; watch :meth:`state_changed` for those. A server-accepted session
        reports a requested drain's result, otherwise raises the session's close reason."""
        await self._inner.closed()

    def state(self) -> ConnectionState:
        """Read the lifecycle phase, connect count, failure, version, and revision together."""
        return self._inner.state()

    async def state_changed(self, after: int) -> ConnectionState:
        """Wait for a revision different from ``after`` without sharing a watcher cursor.

        Pass the revision from this session's last snapshot. Changes between the read
        and wait return immediately. The final snapshot reports ``CLOSED``; waiting
        again at that revision raises the close reason.
        """
        return await self._inner.state_changed(after)

    def cancel(self, code: int) -> None:
        """Close the session with the given error code."""
        self._inner.cancel(code)

    def shutdown(self) -> None:
        """Request a draining close now; await :meth:`closed` for its result.

        Explicit ``moq-lite-07-wip`` waits for the peer to receive finished tails;
        older Lite versions, including the default lite-06, wait only for transport
        ACKs. IETF also waits for transport ACKs.
        """
        self._inner.shutdown()

    def publish(self) -> OriginProducer:
        """The publish-side origin: where local broadcasts are advertised to
        the remote. Either the origin wired before connect/accept, or one
        auto-created if none was set."""
        return OriginProducer._from_inner(self._inner.publish())

    def consume(self) -> OriginConsumer:
        """The subscribe-side origin: a read handle for announcements pushed by
        the remote."""
        return OriginConsumer(self._inner.consume())

    def stats(self) -> ConnectionStats:
        """Snapshot the current connection statistics (RTT, bandwidth estimates,
        byte/packet counters). Cheap to call; intended for periodic polling.

        Individual fields are ``None`` when the transport backend doesn't report them."""
        return ConnectionStats._from_ffi(self._inner.stats())

    def auth(self) -> Auth:
        """The session's authorization: the grant its peer gave it, and more tokens to present.

        Each call returns an independent watcher of the grant. A client session's
        handle survives reconnects, presenting each added token again on every new
        connection."""
        return Auth(self._inner.auth())

    def recv_bandwidth(self) -> BandwidthConsumer | None:
        """Hold an owned receive-estimate consumer; releasing the last one stops PROBE."""
        inner = self._inner.recv_bandwidth()
        return None if inner is None else BandwidthConsumer(inner)

    def allocator(self) -> Allocator:
        """The session's bandwidth allocator.

        Every call returns a handle to the same registry, so reservations made
        through one are visible to the others. A client handle survives
        reconnects: the grant is ``None`` while disconnected and resumes on the
        next connection.
        """
        return Allocator(self._inner.allocator())


class Auth:
    """A session's authorization, returned by :meth:`Session.auth`.

    AUTH is only on ``moq-lite-07-wip`` and on moq-transport sessions that negotiate
    the MoQ Auth extension; elsewhere the grant stays ``None``::

        auth = session.auth()
        print(auth.grant())
        token = await auth.add(fresh_jwt)
    """

    def __init__(self, inner: MoqAuth) -> None:
        self._inner = inner

    def grant(self) -> Grant | None:
        """The union of every token's grant, or ``None`` until the peer first answers one.

        A client session reads ``None`` while it reconnects."""
        return Grant._from_ffi(self._inner.grant())

    async def grant_changed(self) -> Grant | None:
        """Wait for the grant to differ from the one last reported, or held when this
        handle was made, returning it.

        Raises once nothing will change it again: a client's connection or a server's
        session ended. A reconnect is not an end."""
        return Grant._from_ffi(await self._inner.grant_changed())

    async def add(self, token: str) -> Credential:
        """Present another token, returning once the peer accepts it.

        Its grant joins the union until :meth:`Credential.cancel`. The peer's refusal
        raises ``moq.Error`` carrying its code; a session that cannot carry another
        token raises ``moq.Error.Unsupported``."""
        return Credential(await self._inner.add(token))


class Credential:
    """One token presented with :meth:`Auth.add`, withdrawn by :meth:`cancel`."""

    def __init__(self, inner: MoqCredential) -> None:
        self._inner = inner

    def grant(self) -> Grant | None:
        """This token's own grant, or ``None`` after it ended."""
        return Grant._from_ffi(self._inner.grant())

    async def closed(self) -> None:
        """Wait for the token to end, raising the peer's revocation or the end of a client's
        connection or a server's session. A reconnect does not end it."""
        await self._inner.closed()

    def cancel(self) -> None:
        """Withdraw the token, removing its grant from the union."""
        self._inner.cancel()


class BandwidthConsumer:
    """An owned bandwidth estimate, in bits per second, returned by :meth:`Session.recv_bandwidth`."""

    def __init__(self, inner: MoqBandwidthConsumer) -> None:
        self._inner = inner

    def rate(self) -> int | None:
        """The current estimate, or None while unavailable."""
        return self._inner.rate()

    async def rate_changed(self) -> int | None:
        """Wait for the estimate to change and return the new rate, raising when its producer closes."""
        return await self._inner.rate_changed()

    def cancel(self) -> None:
        """Release this consumer's demand, including a pending wait."""
        self._inner.cancel()


class Allocator:
    """Divides one connection's send estimate among the tracks sharing it.

    Minted by :meth:`Session.allocator`. Clones share one reservation registry.
    """

    def __init__(self, inner: MoqAllocator) -> None:
        self._inner = inner

    def reserve(self, demand: TrackDemand, max_bps: int) -> Reservation:
        """Reserve up to ``max_bps`` for the track ``demand`` watches.

        Every track producer hands out its ``demand()``. ``max_bps`` is a
        ceiling, not a measurement: reserve the most the track can ever send.
        Drop the reservation to hand the room back.
        """
        return Reservation(self._inner.reserve(demand._inner, max_bps))


class Reservation:
    """One track's standing claim on an :class:`Allocator`.

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

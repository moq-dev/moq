"""Session wrapper for a MoQ connection, including its initial native dial."""

from __future__ import annotations

from collections.abc import Sequence

from moq_ffi import (
    MoqBandwidth,
    MoqBandwidthConsumer,
    MoqGrantConsumer,
    MoqReservation,
    MoqSession,
    MoqTokens,
)

from ._records import _strs
from .errors import is_shutdown
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

    def grants(self) -> GrantConsumer:
        """Follow the grant the peer gave this session.

        Each call returns an independent consumer. A client session's grant follows
        each reconnect, reading ``None`` while offline."""
        return GrantConsumer(self._inner.grants())

    def tokens(self) -> Tokens | None:
        """The credentials a native client session presents, to replace while it runs.

        ``None`` for a server-accepted session, which presents none."""
        inner = self._inner.tokens()
        return None if inner is None else Tokens(inner)

    def recv_bandwidth(self) -> Bandwidth.Consumer | None:
        """Hold an owned receive-estimate consumer; releasing the last one stops PROBE."""
        inner = self._inner.recv_bandwidth()
        return None if inner is None else Bandwidth.Consumer(inner)

    def bandwidth(self) -> Bandwidth:
        """The session's bandwidth allocator.

        Every call returns a handle to the same registry, so reservations made
        through one are visible to the others. A client handle survives
        reconnects: the grant is ``None`` while disconnected and resumes on the
        next connection.
        """
        return Bandwidth(self._inner.bandwidth())


class GrantConsumer:
    """Follows the grant a session's peer gave it, returned by :meth:`Session.grants`.

    AUTH is only on ``moq-lite-07-wip`` and on moq-transport sessions that negotiate
    the MoQ Auth extension; elsewhere the grant stays ``None``. Iterate it for the
    current grant and then each change, until the session ends::

        async for grant in session.grants():
            print(grant)
    """

    def __init__(self, inner: MoqGrantConsumer) -> None:
        self._inner = inner
        self._started = False

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Grant | None:
        if not self._started:
            self._started = True
            return self.grant()
        try:
            return await self.changed()
        except Exception as err:
            if is_shutdown(err):
                raise StopAsyncIteration from None
            raise

    def grant(self) -> Grant | None:
        """The grant now, or ``None`` until the peer first answers a token.

        A client session reads ``None`` while it reconnects."""
        return Grant._from_ffi(self._inner.grant())

    async def changed(self) -> Grant | None:
        """Wait for the grant to differ from the one this last returned, or held when
        it was made, returning it.

        Raises once nothing will change it again: a client's connection or a server's
        session ended, or :meth:`cancel`. A reconnect is not an end."""
        return Grant._from_ffi(await self._inner.changed())

    def cancel(self) -> None:
        """Cancel all current and future :meth:`changed` calls."""
        self._inner.cancel()


class Tokens:
    """The credentials a native client session presents, returned by :meth:`Session.tokens`."""

    def __init__(self, inner: MoqTokens) -> None:
        self._inner = inner

    def set(self, tokens: Sequence[str]) -> None:
        """Replace the credentials every dial presents, such as before the current ones expire.

        The first is the connection credential, unless the URL carries its own ``?jwt=``;
        each other one widens the grant over in-band AUTH. The next dial presents them,
        and a live session with in-band AUTH presents them at once, withdrawing each
        replaced token once its replacements are answered, even when one is refused.
        After an authorization refusal the session redials only if they changed.
        """
        self._inner.set(_strs(tokens, "tokens"))

    async def answered(self) -> None:
        """Wait for the peer's first verdict on the tokens the last ``set`` newly presented.

        Returns once each is granted, in band or as a later dial's connection credential.
        A token the set kept counts only once refused, and nothing is re-checked after the
        answer. Raises the peer's refusal carrying its code, ``moq.Error.Unsupported`` when
        the session cannot carry an in-band token, or the connection's close reason."""
        await self._inner.answered()


class Bandwidth:
    """Divides one connection's send estimate among the tracks sharing it.

    Minted by :meth:`Session.bandwidth`. Clones share one reservation registry.
    """

    class Consumer:
        """An owned bandwidth estimate, in bits per second."""

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

    def __init__(self, inner: MoqBandwidth) -> None:
        self._inner = inner

    def reserve(self, demand: TrackDemand, max_bps: int) -> Reservation:
        """Reserve up to ``max_bps`` for the track ``demand`` watches.

        Every track producer hands out its ``demand()``. ``max_bps`` is a
        ceiling, not a measurement: reserve the most the track can ever send.
        Drop the reservation to hand the room back.
        """
        return Reservation(self._inner.reserve(demand._inner, max_bps))


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

"""Point-to-point TCP and track sessions over hidden endpoint broadcasts."""

from __future__ import annotations

from moq_ffi import (
    MoqSocketEndpoint,
    MoqSocketIncomingTcp,
    MoqSocketIncomingTrack,
    MoqSocketTcpStream,
    MoqSocketTrackStream,
)

from ._records import _track_info
from .origin import OriginProducer
from .publish import GroupProducer, TrackProducer
from .subscribe import GroupConsumer, TrackConsumer
from .types import TrackInfo

__all__ = ["Endpoint", "IncomingTcp", "IncomingTrack", "TcpStream", "TrackStream"]


class Endpoint:
    """Announces one hidden endpoint and dials or accepts independent sessions.

    Hold it while sessions run. Closing it resets pending dials and sessions.
    Cancelling a dial cancels only that dial; cancelling an accept wait leaves
    the endpoint listening. Usable as an async context manager.
    """

    def __init__(self, origin: OriginProducer, prefix: str, id: str) -> None:
        self._inner = MoqSocketEndpoint(origin._inner, prefix, id)

    @property
    def id(self) -> str:
        """This endpoint's id."""
        return self._inner.id()

    @property
    def prefix(self) -> str:
        """The prefix beneath which this endpoint finds peers."""
        return self._inner.prefix()

    async def __aenter__(self) -> Endpoint:
        return self

    async def __aexit__(self, *exc) -> None:
        self.close()

    def close(self) -> None:
        """Retract the endpoint and reset its pending dials and sessions."""
        self._inner.close()

    async def dial_tcp(self, peer: str) -> TcpStream:
        """Dial a new TCP session, waiting until the peer accepts it."""
        return TcpStream(await self._inner.dial_tcp(peer))

    async def accept_tcp(self) -> IncomingTcp:
        """Start listening for TCP and return the next incoming TCP dial."""
        return IncomingTcp(await self._inner.accept_tcp())

    async def dial_track(self, peer: str, *, info: TrackInfo | None = None) -> TrackStream:
        """Dial a track session; omitted info keeps messages without an age limit."""
        return TrackStream(await self._inner.dial_track(peer, _track_info(info)))

    async def accept_track(self) -> IncomingTrack:
        """Start listening for tracks and return the next incoming track dial."""
        return IncomingTrack(await self._inner.accept_track())


class IncomingTcp:
    """An incoming tcp dial, consumed by accepting or rejecting it."""

    def __init__(self, inner: MoqSocketIncomingTcp) -> None:
        self._inner = inner

    @property
    def peer(self) -> str:
        """The claimed peer id; accepting verifies return routability."""
        return self._inner.peer()

    @property
    def session(self) -> str:
        """The random session id, which must stay private to its endpoints."""
        return self._inner.session()

    async def accept(self) -> TcpStream:
        """Accept after verifying that the peer really dialed this session."""
        return TcpStream(await self._inner.accept())

    def reject(self) -> None:
        """Refuse this dial as not found."""
        self._inner.reject()


class TcpStream:
    """A tcp session's two halves and its peer and session identities."""

    def __init__(self, inner: MoqSocketTcpStream) -> None:
        self._producer = GroupProducer(inner.producer())
        self._consumer = GroupConsumer(inner.consumer())
        self._peer = inner.peer()
        self._session = inner.session()

    @property
    def producer(self) -> GroupProducer:
        """This side's write half."""
        return self._producer

    @property
    def consumer(self) -> GroupConsumer:
        """The peer's read half, retaining its reliable ascending subscription."""
        return self._consumer

    @property
    def peer(self) -> str:
        """The peer endpoint's id."""
        return self._peer

    @property
    def session(self) -> str:
        """The random session id, which must stay private to its endpoints."""
        return self._session


class IncomingTrack:
    """An incoming track dial, consumed by accepting or rejecting it."""

    def __init__(self, inner: MoqSocketIncomingTrack) -> None:
        self._inner = inner

    @property
    def peer(self) -> str:
        """The claimed peer id; accepting verifies return routability."""
        return self._inner.peer()

    @property
    def session(self) -> str:
        """The random session id, which must stay private to its endpoints."""
        return self._inner.session()

    async def accept(self, *, info: TrackInfo | None = None) -> TrackStream:
        """Accept after verifying that the peer really dialed this session."""
        return TrackStream(await self._inner.accept(_track_info(info)))

    def reject(self) -> None:
        """Refuse this dial as not found."""
        self._inner.reject()


class TrackStream:
    """A track session's two halves and its peer and session identities."""

    def __init__(self, inner: MoqSocketTrackStream) -> None:
        self._producer = TrackProducer(inner.producer())
        self._consumer = TrackConsumer(inner.consumer())
        self._peer = inner.peer()
        self._session = inner.session()

    @property
    def producer(self) -> TrackProducer:
        """This side's write half."""
        return self._producer

    @property
    def consumer(self) -> TrackConsumer:
        """The peer's read half, retaining its reliable ascending subscription."""
        return self._consumer

    @property
    def peer(self) -> str:
        """The peer endpoint's id."""
        return self._peer

    @property
    def session(self) -> str:
        """The random session id, which must stay private to its endpoints."""
        return self._session

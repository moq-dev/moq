"""Session wrapper for an established MoQ connection."""

from __future__ import annotations

from moq_ffi import MoqSession

from .origin import OriginConsumer, OriginProducer
from .types import ConnectionStats


class Session:
    """An established MoQ connection, returned by `Client` and `Request.accept()`.

    Hold the session to keep the connection alive; dropping it closes the
    connection. As an async context manager it shuts down gracefully on exit:

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
        """Wait until the session is closed by either side."""
        await self._inner.closed()

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

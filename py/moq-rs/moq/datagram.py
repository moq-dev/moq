"""Live datagram tracks with their own producers, consumers, and requests."""

from __future__ import annotations

from moq_ffi import MoqDatagramConsumer, MoqDatagramProducer, MoqDatagramRequest, MoqDatagramRequests

from ._records import _opt_ts
from ._stream import _Stream
from .types import Datagram, DatagramInfo, Timestamp


class DatagramProducer:
    """Writes live datagrams with an independent sequence counter and no cache."""

    def __init__(self, inner: MoqDatagramProducer) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The datagram track's name within its broadcast."""
        return self._inner.name()

    def info(self) -> DatagramInfo:
        """The publisher properties fixed when this track was created."""
        return self._inner.info()

    def append(self, payload: bytes, *, timestamp: Timestamp | None = None) -> int:
        """Send a datagram and return its sequence; keep the payload below the path MTU."""
        return self._inner.append(_opt_ts(timestamp), payload)

    def insert(self, datagram: Datagram) -> None:
        """Send a datagram at its explicit sequence; appends continue past the highest one."""
        self._inner.insert(datagram._ffi())

    def consume(self) -> DatagramConsumer:
        """Create a local consumer starting with the next datagram written."""
        return DatagramConsumer(self._inner.consume())

    def priority(self) -> int | None:
        """The highest subscriber priority, or None while nobody consumes this track."""
        return self._inner.priority()

    async def priority_changed(self, last: int | None) -> int | None:
        """Wait until aggregate subscriber priority differs from last."""
        return await self._inner.priority_changed(last)

    def finish(self) -> None:
        """Finish the track; existing consumers drain their unread datagrams."""
        self._inner.finish()

    def abort(self, error_code: int) -> None:
        """Abort the track with an application error, discarding unread datagrams."""
        self._inner.abort(error_code)


class DatagramConsumer(_Stream[Datagram]):
    """Reads live datagrams in arrival order, starting when this consumer was created."""

    def __init__(self, inner: MoqDatagramConsumer) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The datagram track's name within its broadcast."""
        return self._inner.name()

    async def info(self) -> DatagramInfo:
        """Wait for the publisher to accept the track and return its properties."""
        return await self._inner.info()

    async def recv(self) -> Datagram | None:
        """Receive the next datagram, or None after the track finishes."""
        datagram = await self._inner.recv()
        return None if datagram is None else Datagram._from_ffi(datagram)

    async def _next(self) -> Datagram | None:
        return await self.recv()

    def priority(self) -> int:
        """This consumer's subscriber priority."""
        return self._inner.priority()

    def set_priority(self, priority: int) -> None:
        """Change this consumer's subscriber priority, higher first."""
        self._inner.set_priority(priority)

    def cancel(self) -> None:
        """Cancel reads and release this consumer's demand."""
        self._inner.cancel()


class DatagramRequest:
    """A requested datagram track awaiting acceptance or rejection."""

    def __init__(self, inner: MoqDatagramRequest) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The requested datagram track's name."""
        return self._inner.name()

    def priority(self) -> int | None:
        """The highest priority among consumers waiting for this track."""
        return self._inner.priority()

    def accept(self, info: DatagramInfo | None = None) -> DatagramProducer:
        """Accept the request with its fixed publisher properties."""
        return DatagramProducer(self._inner.accept(info))

    def abort(self, error_code: int) -> None:
        """Reject this request with an application error code."""
        self._inner.abort(error_code)


class DatagramRequests(_Stream[DatagramRequest]):
    """Serves on-demand datagram tracks through their own request queue."""

    def __init__(self, inner: MoqDatagramRequests) -> None:
        self._inner = inner

    async def requested(self) -> DatagramRequest:
        """Wait for the next datagram track requested by a consumer."""
        return DatagramRequest(await self._inner.requested())

    async def _next(self) -> DatagramRequest:
        return await self.requested()

    def cancel(self) -> None:
        """Cancel the handler and reject requests it has not handed out."""
        self._inner.cancel()

"""Server wrapper for accepting incoming sessions with automatic origin wiring."""

from __future__ import annotations

import asyncio
from collections.abc import Sequence

from moq_ffi import MoqQuicConfig, MoqRequest, MoqServer, MoqServerConfig, MoqServerTls, MoqTransport

from .origin import OriginProducer
from .publish import BroadcastProducer
from .session import Session

# The network transport carrying an incoming session.
Transport = MoqTransport


class Request:
    """Wraps MoqRequest, an incoming session that can be accepted or rejected.

    Use `await request.accept(publish=None, consume=None)` to complete the handshake, or
    `await request.reject(code)` to reject with an application error code.

    Dropping a Request without responding closes the underlying connection
    silently; call `reject(code)` to send an explicit MoQ error.
    """

    def __init__(self, inner: MoqRequest) -> None:
        self._inner = inner

    @property
    def url(self) -> str | None:
        """The URL the client connected to, or `None` if the transport carries none."""
        return self._inner.url()

    @property
    def path(self) -> str:
        """The query-free request path, or an empty string for root/missing."""
        return self._inner.path()

    @property
    def query(self) -> str | None:
        """The encoded request query without `?`; it may contain credentials."""
        return self._inner.query()

    @property
    def transport(self) -> Transport:
        """The network transport carrying this session."""
        return self._inner.transport()

    async def accept(self, *, publish: OriginProducer | None = None, consume: OriginProducer | None = None) -> Session:
        """Complete the MoQ handshake and return the established session.

        None inherits the server origin; a supplied origin replaces it.
        Pass a fresh origin for isolation, or the same origin for both sides.

        The caller must hold the returned session to keep the connection
        alive; dropping it closes the session. Raises `Error.AlreadyResponded`
        if `accept()` or `reject()` has already been called.
        """
        return Session(
            await self._inner.accept(
                publish=publish._inner if publish is not None else None,
                consume=consume._inner if consume is not None else None,
            )
        )

    async def reject(self, code: int) -> None:
        """Reject the session with the given application error code.

        Codes 401 and 403 map to the protocol's unauthorized error.

        Raises `Error.AlreadyResponded` if `accept()` or `reject()` has already
        been called.
        """
        await self._inner.reject(code)

    def cancel(self) -> None:
        """Cancel an in-flight `accept()` or `reject()` call."""
        self._inner.cancel()


class Server:
    """High-level MoQ server with automatic origin wiring.

    In simple mode (no origin provided), creates an internal origin automatically::

        async with Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
            broadcast = server.create_broadcast("live")
            broadcast.announce()  # unannounced broadcasts are invisible
            await server.serve()

    Or hand-roll the accept loop if you need per-request control::

        async with Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
            async for request in server:
                if request.path == "/admin":
                    await request.reject(403)
                    continue
                session = await request.accept(publish=None, consume=None)  # hold to keep the connection alive

    Exiting the context manager stops accepting new sessions and releases the
    listening socket before it returns, so the address can be bound again
    immediately. In-flight sessions stay alive until their handles are dropped
    or `Session.cancel()` is called.

    In advanced mode, provide your own origins for full control::

        origin = OriginProducer()
        server = Server(
            "127.0.0.1:4443",
            tls_generate=["localhost"],
            publish=origin,
            consume=origin,
        )
    """

    def __init__(
        self,
        bind: str = "[::]:443",
        *,
        tls_cert: Sequence[str] = (),
        tls_key: Sequence[str] = (),
        tls_generate: Sequence[str] = (),
        versions: Sequence[str] = (),
        max_streams: int | None = None,
        publish: OriginProducer | None = None,
        consume: OriginProducer | None = None,
    ) -> None:
        # If neither origin is provided, create a shared internal one.
        if publish is None and consume is None:
            publish = consume = OriginProducer()
        self._publish_origin = publish

        self._config = MoqServerConfig(
            bind=bind,
            versions=list(versions),
            tls=MoqServerTls(cert=list(tls_cert), key=list(tls_key), generate=list(tls_generate)),
            quic=MoqQuicConfig(max_streams=max_streams),
            publish=None if publish is None else publish._inner,
            consume=None if consume is None else consume._inner,
        )

        self._inner: MoqServer | None = None
        self._local_addr: str | None = None

    async def __aenter__(self):
        self._inner = MoqServer(self._config)
        try:
            self._local_addr = await self._inner.listen()
        except BaseException:
            self._inner.cancel()
            self._inner = None
            raise
        return self

    async def __aexit__(self, *exc) -> None:
        if self._inner is not None:
            self._inner.cancel()
            self._inner = None
        self._local_addr = None

    @property
    def local_addr(self) -> str:
        """The bound local address, available after entering the context manager."""
        if self._local_addr is None:
            raise RuntimeError("server not listening; use 'async with'")
        return self._local_addr

    def cert_fingerprints(self) -> list[str]:
        """SHA-256 fingerprints of the configured TLS certificates, hex-encoded.

        Useful for pinning a generated self-signed certificate in a browser
        via WebTransport's `serverCertificateHashes`.
        """
        if self._inner is None:
            raise RuntimeError("server not listening; use 'async with'")
        return self._inner.cert_fingerprints()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Request:
        if self._inner is None:
            raise RuntimeError("server not listening; use 'async with'")
        request = await self._inner.accept()
        if request is None:
            raise StopAsyncIteration
        return Request(request)

    def create_broadcast(self, path: str) -> BroadcastProducer:
        """Create a live broadcast at ``path``, served to incoming sessions.

        See :meth:`OriginProducer.create_broadcast`.
        """
        origin = self._publish_origin
        if origin is None:
            raise RuntimeError("no publish origin configured")
        return origin.create_broadcast(path)

    async def serve(self) -> None:
        """Accept every session in a loop, holding each one alive until it closes.

        Each session is handled by its own task that awaits `session.closed()`,
        so memory does not grow with the number of past connections.

        To inspect or reject requests, iterate the server directly instead:

            async for request in server:
                if request.path == "/admin":
                    await request.reject(403)
                    continue
                session = await request.accept(publish=None, consume=None)
        """
        session_tasks: set[asyncio.Task] = set()

        async def serve_session(request: Request) -> None:
            session = await request.accept(publish=None, consume=None)
            await session.closed()

        try:
            async for request in self:
                task = asyncio.create_task(serve_session(request))
                session_tasks.add(task)
                task.add_done_callback(session_tasks.discard)
        finally:
            # Cancel any session tasks still pending a handshake, but let
            # established sessions wind down on their own.
            for task in list(session_tasks):
                if not task.done():
                    task.cancel()

"""Server wrapper — accept incoming sessions with automatic origin wiring."""

from __future__ import annotations

from collections.abc import Sequence

from moq_ffi import MoqRequest, MoqServer

from .origin import OriginProducer
from .publish import BroadcastProducer


class Request:
    """Wraps MoqRequest — an incoming session that can be accepted or rejected.

    Use `await request.ok()` to complete the handshake, or
    `await request.close(code)` to reject with an HTTP status code.
    """

    def __init__(self, inner: MoqRequest) -> None:
        self._inner = inner

    @property
    def url(self) -> str | None:
        return self._inner.url()

    @property
    def transport(self) -> str:
        return self._inner.transport()

    def set_publish(self, origin: OriginProducer | None) -> None:
        """Override the publish origin for this session. Falls back to the
        server's configured publish origin if unset."""
        self._inner.set_publish(origin._inner if origin is not None else None)

    def set_consume(self, origin: OriginProducer | None) -> None:
        """Override the consume origin for this session. Falls back to the
        server's configured consume origin if unset."""
        self._inner.set_consume(origin._inner if origin is not None else None)

    async def ok(self):
        """Complete the MoQ handshake and return the established session.

        The caller must hold the returned session to keep the connection
        alive; dropping it closes the session.
        """
        return await self._inner.ok()

    async def close(self, code: int = 404) -> None:
        """Reject the session with the given HTTP status code."""
        await self._inner.close(code)


class Server:
    """High-level MoQ server with automatic origin wiring.

    In simple mode (no origin provided), creates an internal origin automatically:

        async with Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
            server.publish("live", broadcast)
            async for request in server:
                session = await request.ok()
                # hold `session` to keep the connection alive

    In advanced mode, provide your own origins for full control:

        origin = OriginProducer()
        server = Server(
            "127.0.0.1:4443",
            tls_generate=["localhost"],
            publish=origin,
            subscribe=origin,
        )
    """

    def __init__(
        self,
        bind: str = "[::]:443",
        *,
        tls_cert: Sequence[str] = (),
        tls_key: Sequence[str] = (),
        tls_generate: Sequence[str] = (),
        publish: OriginProducer | None = None,
        subscribe: OriginProducer | None = None,
    ) -> None:
        self._bind = bind
        self._tls_cert = list(tls_cert)
        self._tls_key = list(tls_key)
        self._tls_generate = list(tls_generate)

        # If neither origin is provided, create a shared internal one.
        if publish is None and subscribe is None:
            self._origin: OriginProducer | None = OriginProducer()
            self._publish_origin: OriginProducer | None = self._origin
            self._consume_origin: OriginProducer | None = self._origin
        else:
            self._origin = None
            self._publish_origin = publish
            self._consume_origin = subscribe

        self._inner: MoqServer | None = None
        self._local_addr: str | None = None

    async def __aenter__(self):
        self._inner = MoqServer()
        self._inner.set_bind(self._bind)
        if self._tls_cert:
            self._inner.set_tls_cert(self._tls_cert)
        if self._tls_key:
            self._inner.set_tls_key(self._tls_key)
        if self._tls_generate:
            self._inner.set_tls_generate(self._tls_generate)
        if self._publish_origin is not None:
            self._inner.set_publish(self._publish_origin._inner)
        if self._consume_origin is not None:
            self._inner.set_consume(self._consume_origin._inner)

        self._local_addr = await self._inner.listen()
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

    def __aiter__(self):
        return self

    async def __anext__(self) -> Request:
        if self._inner is None:
            raise RuntimeError("server not listening; use 'async with'")
        request = await self._inner.accept()
        if request is None:
            raise StopAsyncIteration
        return Request(request)

    def publish(self, path: str, broadcast: BroadcastProducer) -> None:
        """Publish a broadcast under the given path, served to incoming sessions."""
        origin = self._publish_origin
        if origin is None:
            raise RuntimeError("no publish origin configured")
        origin.publish(path, broadcast)

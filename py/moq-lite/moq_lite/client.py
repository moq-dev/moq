"""Client wrapper — simplified connection with automatic origin wiring."""

from __future__ import annotations

from moq_ffi import MoqClient

from .origin import Announced, AnnouncedBroadcast, OriginConsumer, OriginProducer
from .publish import BroadcastProducer


class Client:
    """High-level MoQ client with automatic origin wiring.

    In simple mode (no origin provided), creates an internal origin automatically:

        async with Client("https://relay.example.com") as client:
            async for ann in client.announced():
                ...

    In advanced mode, provide your own origin for full control:

        origin = OriginProducer()
        client = Client("https://relay.example.com", publish=origin, subscribe=origin)
    """

    def __init__(
        self,
        url: str,
        *,
        tls_verify: bool = True,
        publish: OriginProducer | None = None,
        subscribe: OriginProducer | None = None,
    ) -> None:
        self._url = url
        self._tls_verify = tls_verify

        # If neither origin is provided, create a shared internal one.
        if publish is None and subscribe is None:
            self._origin = OriginProducer()
            self._publish_origin = self._origin
            self._consume_origin = self._origin
        else:
            self._origin = None
            self._publish_origin = publish
            self._consume_origin = subscribe

        self._consumer: OriginConsumer | None = None
        self._inner: MoqClient | None = None
        self._session = None

    async def __aenter__(self):
        self._inner = MoqClient()

        if not self._tls_verify:
            self._inner.set_tls_disable_verify(True)

        if self._publish_origin is not None:
            self._inner.set_publish(self._publish_origin._inner)
        if self._consume_origin is not None:
            self._inner.set_consume(self._consume_origin._inner)

        self._session = await self._inner.connect(self._url)

        # Create consumer from whichever origin handles consuming.
        origin = self._consume_origin or self._publish_origin
        if origin is not None:
            self._consumer = origin.consume()

        return self

    async def __aexit__(self, *exc) -> None:
        self._consumer = None
        if self._inner is not None:
            self._inner.cancel()
            self._inner = None
        self._session = None

    def publish(self, path: str, broadcast: BroadcastProducer) -> None:
        origin = self._publish_origin
        if origin is None:
            raise RuntimeError("no publish origin configured")
        origin.publish(path, broadcast)

    def announced(self, prefix: str = "") -> Announced:
        if self._consumer is None:
            raise RuntimeError("no consume origin configured")
        return self._consumer.announced(prefix)

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        if self._consumer is None:
            raise RuntimeError("no consume origin configured")
        return self._consumer.announced_broadcast(path)

    @property
    def session(self):
        return self._session

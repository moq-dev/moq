"""Client wrapper for simplified connection with automatic origin wiring."""

from __future__ import annotations

from moq_ffi import MoqClient

from .origin import Announced, AnnouncedBroadcast, OriginConsumer, OriginProducer
from .publish import BroadcastProducer
from .session import Session
from .subscribe import BroadcastConsumer


class Client:
    """High-level MoQ client with automatic origin wiring.

    In simple mode (no origin provided), both sides share one origin, so a broadcast
    announced here is also discoverable here:

        async with Client("https://relay.example.com") as client:
            async for ann in client.announced():
                ...

    In advanced mode, provide your own origin for full control:

        origin = OriginProducer()
        client = Client("https://relay.example.com", publish=origin, subscribe=origin)

    For a relay that requires mTLS, pass a paired client certificate and key:

        client = Client("https://relay.example.com", tls_cert="client.pem", tls_key="client.key")
    """

    def __init__(
        self,
        url: str,
        *,
        tls_verify: bool = True,
        tls_roots: list[str] | None = None,
        tls_system_roots: bool | None = None,
        tls_fingerprints: list[str] | None = None,
        tls_cert: str | None = None,
        tls_key: str | None = None,
        bind: str | None = None,
        publish: OriginProducer | None = None,
        subscribe: OriginProducer | None = None,
    ) -> None:
        self._url = url
        self._tls_verify = tls_verify
        self._tls_roots = tls_roots
        self._tls_system_roots = tls_system_roots
        self._tls_fingerprints = tls_fingerprints
        self._tls_cert = tls_cert
        self._tls_key = tls_key
        self._bind = bind

        # With neither side given, moq-ffi wires one shared origin to both, so a broadcast
        # announced here is discoverable via announced() (loopback).
        self._publish_origin = publish
        self._consume_origin = subscribe

        self._publisher: OriginProducer | None = None
        self._consumer: OriginConsumer | None = None
        self._inner: MoqClient | None = None
        self._session: Session | None = None

    async def __aenter__(self):
        self._inner = MoqClient()

        if not self._tls_verify:
            self._inner.set_tls_disable_verify(True)
        if self._tls_roots:
            self._inner.set_tls_roots(self._tls_roots)
        if self._tls_system_roots is not None:
            self._inner.set_tls_system_roots(self._tls_system_roots)
        if self._tls_fingerprints:
            self._inner.set_tls_fingerprints(self._tls_fingerprints)
        if self._tls_cert is not None:
            self._inner.set_tls_cert(self._tls_cert)
        if self._tls_key is not None:
            self._inner.set_tls_key(self._tls_key)
        if self._bind is not None:
            self._inner.set_bind(self._bind)

        if self._publish_origin is not None:
            self._inner.set_publish(self._publish_origin._inner)
        if self._consume_origin is not None:
            self._inner.set_consume(self._consume_origin._inner)

        self._session = Session(await self._inner.connect(self._url))

        # The session always exposes both sides, wired from the origins above or
        # auto-created, so publishing and discovery always have somewhere to go.
        self._publisher = self._session.publisher()
        self._consumer = self._session.consumer()

        return self

    async def __aexit__(self, *exc) -> None:
        self._publisher = None
        self._consumer = None
        if self._session is not None:
            self._session.shutdown()
            self._session = None
        if self._inner is not None:
            self._inner.cancel()
            self._inner = None
        self._session = None

    def create_broadcast(self, path: str) -> BroadcastProducer:
        """Create a live broadcast at ``path`` so subscribers can discover it.

        See :meth:`OriginProducer.create_broadcast`.
        """
        return self._require_publisher().create_broadcast(path)

    def announced(self, prefix: str = "") -> Announced:
        return self._require_consumer().announced(prefix)

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        return self._require_consumer().announced_broadcast(path)

    async def request_broadcast(self, path: str) -> BroadcastConsumer:
        """Request a broadcast by path, resolving as soon as it can be served."""
        return await self._require_consumer().request_broadcast(path)

    def _require_publisher(self) -> OriginProducer:
        if self._publisher is None:
            raise RuntimeError("not connected; use the client as an async context manager")
        return self._publisher

    def _require_consumer(self) -> OriginConsumer:
        if self._consumer is None:
            raise RuntimeError("not connected; use the client as an async context manager")
        return self._consumer

    @property
    def session(self) -> Session | None:
        """The established session, or `None` before connecting / after exit."""
        return self._session


def connect(
    url: str,
    *,
    tls_verify: bool = True,
    tls_roots: list[str] | None = None,
    tls_system_roots: bool | None = None,
    tls_fingerprints: list[str] | None = None,
    tls_cert: str | None = None,
    tls_key: str | None = None,
    bind: str | None = None,
    publish: OriginProducer | None = None,
    subscribe: OriginProducer | None = None,
) -> Client:
    """Shorthand for constructing a :class:`Client`.

    Use it directly as an async context manager:

        async with moq.connect("https://relay.example.com") as client:
            ...
    """
    return Client(
        url,
        tls_verify=tls_verify,
        tls_roots=tls_roots,
        tls_system_roots=tls_system_roots,
        tls_fingerprints=tls_fingerprints,
        tls_cert=tls_cert,
        tls_key=tls_key,
        bind=bind,
        publish=publish,
        subscribe=subscribe,
    )

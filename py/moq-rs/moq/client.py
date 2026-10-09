"""Client wrapper for simplified connection with automatic origin wiring."""

from __future__ import annotations

from collections.abc import Sequence
from datetime import timedelta

from moq_ffi import (
    MoqClient,
    MoqClientConfig,
    MoqClientTls,
    MoqQuicConfig,
    MoqWebSocketConfig,
)

from ._records import Backoff, _opt_us, _strs
from .origin import AnnounceConsumer, AnnouncedBroadcast, OriginConsumer, OriginProducer
from .publish import BroadcastProducer
from .session import Session
from .subscribe import BroadcastConsumer


class Client:
    """High-level MoQ client with automatic origin wiring.

    In simple mode (no origin provided), both sides share one origin, so a broadcast
    announced here is also discoverable here:

        async with Client("https://relay.example.com") as client:
            async for event in client.announced():
                ...

    In advanced mode, provide your own origin for full control:

        origin = OriginProducer()
        client = Client("https://relay.example.com", publish=origin, consume=origin)

    The WebSocket fallback races QUIC for ``http(s)`` URLs after a 200 ms head start.
    Pass ``websocket_enabled=False`` against a QUIC-only relay, or a ``websocket_delay``
    to change the head start:

        client = Client("https://relay.example.com", websocket_delay=timedelta(milliseconds=50))

    For a relay that requires mTLS, pass a paired client certificate and key:

        client = Client("https://relay.example.com", tls_cert="client.pem", tls_key="client.key")

    The session automatically reconnects with backoff when the transport drops, and
    broadcasts consumed through it ride out the gap. Pass ``reconnect=False`` for a
    one-shot dial, or a :class:`Backoff` to tune the retry pacing; watch
    :meth:`Session.status` for the connect/disconnect transitions.

    ``versions`` restricts the protocol versions offered, most preferred first, spelled
    like ``"moq-lite-03"``; empty offers every supported version.
    """

    def __init__(
        self,
        url: str,
        *,
        tls_verify: bool = True,
        tls_roots: Sequence[str] = (),
        tls_system_roots: bool | None = None,
        tls_fingerprints: Sequence[str] = (),
        tls_cert: str | None = None,
        tls_key: str | None = None,
        bind: str | None = None,
        versions: Sequence[str] = (),
        max_streams: int | None = None,
        websocket_enabled: bool | None = None,
        websocket_delay: timedelta | None = None,
        reconnect: bool = True,
        backoff: Backoff | None = None,
        publish: OriginProducer | None = None,
        consume: OriginProducer | None = None,
    ) -> None:
        self._url = url
        # With neither side given, moq-ffi wires one shared origin to both, so a broadcast
        # announced here is discoverable via announced() (loopback).
        self._config = MoqClientConfig(
            bind=bind,
            versions=_strs(versions, "versions"),
            tls=MoqClientTls(
                insecure=not tls_verify,
                roots=_strs(tls_roots, "tls_roots"),
                system_roots=tls_system_roots,
                fingerprints=_strs(tls_fingerprints, "tls_fingerprints"),
                cert=tls_cert,
                key=tls_key,
            ),
            quic=MoqQuicConfig(max_streams=max_streams),
            websocket=MoqWebSocketConfig(
                enabled=websocket_enabled,
                delay_us=_opt_us(websocket_delay, "websocket_delay"),
            ),
            once=not reconnect,
            backoff=(backoff or Backoff())._ffi(),
            publish=None if publish is None else publish._inner,
            consume=None if consume is None else consume._inner,
        )

        self._publisher: OriginProducer | None = None
        self._consumer: OriginConsumer | None = None
        self._inner: MoqClient | None = None
        self._session: Session | None = None

    async def __aenter__(self):
        self._inner = MoqClient(self._config)
        try:
            self._session = Session(await self._inner.connect(self._url))
        except BaseException:
            self._inner.cancel()
            self._inner = None
            raise

        # The session always exposes both sides, wired from the origins above or
        # auto-created, so publishing and discovery always have somewhere to go.
        self._publisher = self._session.publish()
        self._consumer = self._session.consume()

        return self

    async def __aexit__(self, *exc) -> None:
        self._publisher = None
        self._consumer = None
        try:
            if self._session is not None:
                # A body error is the failure worth reporting. Draining behind it would
                # wait out the deadline and replace it with a delivery timeout. Cancel
                # rather than skip: dropping the last session closes the transport, so
                # one the caller kept a reference to would otherwise stay open.
                if exc[0] is None:
                    await self._session.shutdown()
                else:
                    self._session.cancel(0)
        finally:
            self._session = None
            if self._inner is not None:
                self._inner.cancel()
                self._inner = None

    def create_broadcast(self, path: str) -> BroadcastProducer:
        """Create an unannounced broadcast at ``path``, invisible until announced. Announce it after populating tracks.

        See :meth:`OriginProducer.create_broadcast`.
        """
        return self._require_publisher().create_broadcast(path)

    def announced(self, prefix: str = "", *, filter: str | None = None, hidden: bool = False) -> AnnounceConsumer:
        """Async-iterate broadcasts under ``prefix`` matching an optional pattern.

        See :meth:`OriginConsumer.announced`.
        """
        return self._require_consumer().announced(prefix, filter=filter, hidden=hidden)

    def announced_broadcast(self, path: str) -> AnnouncedBroadcast:
        """Await announcement of the broadcast at exactly ``path``.

        See :meth:`OriginConsumer.announced_broadcast`.
        """
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
    tls_roots: Sequence[str] = (),
    tls_system_roots: bool | None = None,
    tls_fingerprints: Sequence[str] = (),
    tls_cert: str | None = None,
    tls_key: str | None = None,
    bind: str | None = None,
    versions: Sequence[str] = (),
    max_streams: int | None = None,
    websocket_enabled: bool | None = None,
    websocket_delay: timedelta | None = None,
    reconnect: bool = True,
    backoff: Backoff | None = None,
    publish: OriginProducer | None = None,
    consume: OriginProducer | None = None,
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
        versions=versions,
        max_streams=max_streams,
        websocket_enabled=websocket_enabled,
        websocket_delay=websocket_delay,
        reconnect=reconnect,
        backoff=backoff,
        publish=publish,
        consume=consume,
    )

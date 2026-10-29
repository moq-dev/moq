"""Client wrapper for simplified connection with automatic origin wiring."""

from __future__ import annotations

from collections.abc import Sequence
from datetime import timedelta

from moq_ffi import (
    MoqClient,
    MoqClientConfig,
    MoqCongestionControl,
    moq_qlog_supported,
    moq_versions,
)

from ._records import Backoff, ClientTls, QuicConfig, WebSocketConfig, _implementation, _strs
from .origin import AnnounceConsumer, AnnouncedBroadcast, OriginConsumer, OriginProducer, Publication
from .session import Session
from .subscribe import BroadcastConsumer

CongestionControl = MoqCongestionControl


def versions() -> list[str]:
    """The protocol versions offered by default, most preferred first, as ``versions`` spells them."""
    return moq_versions()


def qlog_supported() -> bool:
    """Whether this build can write qlog traces; ``qlog`` raises `moq.Error` otherwise."""
    return moq_qlog_supported()


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
    Pass ``websocket=WebSocketConfig(enabled=False)`` against a QUIC-only relay, or a
    ``delay`` to change the head start:

        client = Client("https://relay.example.com", websocket=WebSocketConfig(delay=timedelta(milliseconds=50)))

    For a relay that requires mTLS, pass a paired client certificate and key:

        client = Client("https://relay.example.com", tls=ClientTls(cert="client.pem", key="client.key"))

    The session automatically reconnects with backoff when the transport drops, and
    broadcasts consumed through it ride out the gap. Pass ``once=True`` for a one-shot
    dial, or a :class:`Backoff` to tune the retry pacing; watch
    :meth:`Session.state_changed` for the connect/disconnect transitions.

    ``versions`` restricts the protocol versions offered, most preferred first, spelled
    like ``"moq-lite-03"``; empty offers every supported version, which :func:`versions` lists.

    ``tokens`` are credentials every dial presents. The first is the connection credential,
    sent in SETUP and, while an offered version can't carry it there, the URL's ``?jwt=``;
    each other one widens the session's grant over in-band AUTH. A URL that already carries
    ``?jwt=`` keeps it, and then every token rides AUTH. Replace them before they expire
    through :meth:`tokens`.

    ``timeout`` bounds one connect, dial and handshake together (zero waits forever).
    ``race`` and ``resolution_delay`` pace Happy Eyeballs. ``tls`` sets certificate trust
    and the mTLS identity (:class:`ClientTls`), and ``quic`` tunes the transport
    (:class:`QuicConfig`). ``None`` keeps the library default throughout.
    """

    def __init__(
        self,
        url: str,
        *,
        bind: str | None = None,
        versions: Sequence[str] = (),
        tokens: Sequence[str] = (),
        timeout: timedelta | None = None,
        race: timedelta | None = None,
        resolution_delay: timedelta | None = None,
        tls: ClientTls | None = None,
        quic: QuicConfig | None = None,
        websocket: WebSocketConfig | None = None,
        once: bool = False,
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
            tokens=_strs(tokens, "tokens"),
            timeout=timeout,
            race=race,
            resolution_delay=resolution_delay,
            tls=(tls or ClientTls())._ffi(),
            quic=(quic or QuicConfig())._ffi(),
            websocket=(websocket or WebSocketConfig())._ffi(),
            once=once,
            backoff=(backoff or Backoff())._ffi(),
            implementation=_implementation(),
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
                    self._session.shutdown()
                    await self._session.closed()
                else:
                    self._session.cancel(0)
        finally:
            self._session = None
            if self._inner is not None:
                self._inner.cancel()
                self._inner = None

    def create_broadcast(self, path: str) -> Publication:
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
        """Follow the broadcast at exactly ``path`` across its publisher instances.

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
        """The connecting or connected session, or `None` before connecting / after exit."""
        return self._session


def connect(
    url: str,
    *,
    bind: str | None = None,
    versions: Sequence[str] = (),
    tokens: Sequence[str] = (),
    timeout: timedelta | None = None,
    race: timedelta | None = None,
    resolution_delay: timedelta | None = None,
    tls: ClientTls | None = None,
    quic: QuicConfig | None = None,
    websocket: WebSocketConfig | None = None,
    once: bool = False,
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
        bind=bind,
        versions=versions,
        tokens=tokens,
        timeout=timeout,
        race=race,
        resolution_delay=resolution_delay,
        tls=tls,
        quic=quic,
        websocket=websocket,
        once=once,
        backoff=backoff,
        publish=publish,
        consume=consume,
    )

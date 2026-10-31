"""Root records the wrapper owns, as frozen dataclasses with Python defaults.

moq-ffi spells media timestamps as integer microseconds, so ``Frame`` and
``Datagram`` convert them to ``timedelta`` at the boundary. Durations already
cross as ``timedelta``. ``ClientTls`` lives here with the rest of the client
config, so a bare string passed for a list is refused instead of splitting into
characters.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import timedelta
from importlib import metadata

from moq_ffi import (
    MoqBackoff,
    MoqClientTls,
    MoqCongestionControl,
    MoqConnectionStats,
    MoqDatagram,
    MoqFrame,
    MoqGrant,
    MoqOrder,
    MoqQuicConfig,
    MoqRange,
    MoqSubscription,
    MoqTrackInfo,
    MoqWebSocketConfig,
    moq_subscription_defaults,
)

_MICROSECOND = timedelta(microseconds=1)


def _implementation() -> str:
    """This package's product token, which its sessions advertise ahead of moq-ffi's."""
    try:
        return f"moq-py/{metadata.version('moq-rs')}"
    except metadata.PackageNotFoundError:
        # Imported from a source tree rather than an installed distribution.
        return "moq-py/dev"


def _to_us(value: timedelta, name: str) -> int:
    if value < timedelta(0):
        raise ValueError(f"{name} must not be negative: {value}")
    return value // _MICROSECOND


def _opt_us(value: timedelta | None, name: str) -> int | None:
    return None if value is None else _to_us(value, name)


def _from_us(us: int) -> timedelta:
    return timedelta(microseconds=us)


def _strs(value: Sequence[str], name: str) -> list[str]:
    # A str is itself a Sequence[str], so list() would split it into characters.
    if isinstance(value, str):
        raise TypeError(f"{name} takes a sequence of strings, not a str: {value!r}")
    return list(value)


def _opt_from_us(us: int | None) -> timedelta | None:
    return None if us is None else _from_us(us)


@dataclass(frozen=True)
class Frame:
    """A raw track frame: a payload and its presentation timestamp.

    ``timestamp`` is ``None`` on an untimed track, on both publish and receive.
    """

    payload: bytes
    timestamp: timedelta | None = None

    def _ffi(self) -> MoqFrame:
        return MoqFrame(payload=self.payload, timestamp_us=_opt_us(self.timestamp, "timestamp"))

    @staticmethod
    def _from_ffi(frame: MoqFrame) -> Frame:
        return Frame(payload=frame.payload, timestamp=_opt_from_us(frame.timestamp_us))


@dataclass(frozen=True)
class Datagram:
    """A best-effort track datagram as received: sequence number, timestamp, and payload.

    ``timestamp`` is ``None`` on a datagram read from an untimed track.
    """

    sequence: int
    timestamp: timedelta | None
    payload: bytes

    @staticmethod
    def _from_ffi(datagram: MoqDatagram) -> Datagram:
        return Datagram(
            sequence=datagram.sequence,
            timestamp=_opt_from_us(datagram.timestamp_us),
            payload=datagram.payload,
        )


@dataclass(frozen=True)
class Subscription:
    """Subscriber-side delivery preferences, mirroring moq-net's ``track::Subscription``.

    ``priority`` is in ``-128..=127``, higher first, and defaults to 0, the midpoint.
    ``max_delay`` is how far a non-latest group may fall behind before it is skipped;
    zero skips at once, ``None`` sets no subscriber limit and leaves the publisher's max
    age alone to bound it, and it caps every range. ``ranges`` are the spans of the track
    to deliver, ``None`` for everything ``max_delay`` still allows. ``order`` is which
    pending group the publisher sends first, ``None`` for newest first.

    Start from :meth:`default` for moq-net's defaults; a bare ``Subscription()`` sets no
    max delay.
    """

    priority: int = 0
    max_delay: timedelta | None = None
    ranges: Sequence[MoqRange] | None = None
    order: MoqOrder | None = None

    @classmethod
    def default(cls) -> Subscription:
        """moq-net's defaults, whose zero ``max_delay`` skips a superseded group at once.

        A ``None`` subscription means the same; ``dataclasses.replace`` this one to change
        a field.
        """
        defaults = moq_subscription_defaults()
        return cls(
            priority=defaults.priority,
            max_delay=defaults.max_delay,
            ranges=defaults.ranges,
            order=defaults.order,
        )

    def _ffi(self) -> MoqSubscription:
        return MoqSubscription(
            priority=self.priority,
            max_delay=self.max_delay,
            ranges=None if self.ranges is None else list(self.ranges),
            order=self.order,
        )


def _subscription(subscription: Subscription | None) -> MoqSubscription | None:
    return None if subscription is None else subscription._ffi()


@dataclass(frozen=True)
class TrackInfo:
    """Publisher-side track properties, mirroring moq-net's ``track::Info``.

    ``priority`` is in ``-128..=127``, higher first, and defaults to 0, the midpoint. It breaks
    ties between subscriptions of equal subscriber priority. ``max_age`` is how long the
    publisher caches a non-latest group, ``None`` for no limit. ``refresh`` is how many
    times its first frame a group's later bytes may grow before a joining subscriber
    asks for a new group, ``None`` to never ask. ``timescale`` is ticks per second:
    ``None`` means the track is untimed, on both publish and receive.
    ``compress`` DEFLATEs each group on the wire to peers that support it; readers
    always see the original frames.
    """

    priority: int = 0
    max_age: timedelta | None = None
    refresh: int | None = None
    timescale: int | None = None
    compress: bool = False

    def _ffi(self) -> MoqTrackInfo:
        return MoqTrackInfo(
            priority=self.priority,
            max_age=self.max_age,
            refresh=self.refresh,
            timescale=self.timescale,
            compress=self.compress,
        )

    @staticmethod
    def _from_ffi(info: MoqTrackInfo) -> TrackInfo:
        return TrackInfo(
            priority=info.priority,
            max_age=info.max_age,
            refresh=info.refresh,
            timescale=info.timescale,
            compress=info.compress,
        )


def _track_info(info: TrackInfo | None) -> MoqTrackInfo | None:
    return None if info is None else info._ffi()


@dataclass(frozen=True)
class Backoff:
    """Retry pacing for the automatic reconnect.

    The delay starts at ``initial``, multiplies by ``multiplier`` after each failed
    attempt, and caps at ``max``. After ``timeout`` of consecutive failures the
    connection gives up; ``timedelta(0)`` retries forever. ``None`` keeps each
    default: 1s, x2, 5s, and a 10s window.
    """

    initial: timedelta | None = None
    multiplier: int | None = None
    max: timedelta | None = None
    timeout: timedelta | None = None

    def _ffi(self) -> MoqBackoff:
        return MoqBackoff(
            initial=self.initial,
            multiplier=self.multiplier,
            max=self.max,
            timeout=self.timeout,
        )


@dataclass(frozen=True)
class ClientTls:
    """Certificate trust and the mTLS identity for a :class:`Client`.

    ``insecure`` skips certificate verification, for local development only. ``roots``
    are PEM root certificate files to trust instead of the platform roots, and
    ``system_roots`` whether to also trust the platform roots (``None`` trusts them only
    when ``roots`` is empty). ``fingerprints`` pin the peer to hex SHA-256 certificate
    fingerprints, accepting what :meth:`Server.cert_fingerprints` reports. ``cert`` and
    ``key`` are the paired PEM chain and private key to present for mTLS. ``host_name``
    verifies the certificate against another name (SNI) than the URL's host.
    """

    insecure: bool = False
    roots: Sequence[str] = ()
    system_roots: bool | None = None
    fingerprints: Sequence[str] = ()
    cert: str | None = None
    key: str | None = None
    host_name: str | None = None

    def _ffi(self) -> MoqClientTls:
        return MoqClientTls(
            insecure=self.insecure,
            roots=_strs(self.roots, "roots"),
            system_roots=self.system_roots,
            fingerprints=_strs(self.fingerprints, "fingerprints"),
            cert=self.cert,
            key=self.key,
            host_name=self.host_name,
        )


@dataclass(frozen=True)
class QuicConfig:
    """QUIC transport tuning for a :class:`Client`; the WebSocket fallback ignores it.

    ``max_streams`` caps the concurrent streams the peer may open toward this endpoint
    (0 is refused); MoQ opens one per group, so a subscriber to many tracks wants it
    raised. ``idle_timeout`` is how long an idle connection survives, and ``keep_alive``
    the ping interval (zero disables it). ``gso``, ``mtu_discovery``, and
    ``congestion_control`` leave the choice to the backend when ``None``. ``qlog`` is a
    directory, which must exist, for qlog traces when :func:`qlog_supported`.
    ``keylog=True`` writes TLS session secrets to the file ``SSLKEYLOGFILE`` names, for
    decrypting a capture; without ``SSLKEYLOGFILE`` the client refuses to build.
    ``None`` keeps the library default throughout.
    """

    max_streams: int | None = None
    idle_timeout: timedelta | None = None
    keep_alive: timedelta | None = None
    gso: bool | None = None
    mtu_discovery: bool | None = None
    congestion_control: MoqCongestionControl | None = None
    qlog: str | None = None
    keylog: bool = False

    def _ffi(self) -> MoqQuicConfig:
        return MoqQuicConfig(
            max_streams=self.max_streams,
            idle_timeout=self.idle_timeout,
            keep_alive=self.keep_alive,
            gso=self.gso,
            mtu_discovery=self.mtu_discovery,
            congestion_control=self.congestion_control,
            qlog=self.qlog,
            keylog=self.keylog,
        )


@dataclass(frozen=True)
class WebSocketConfig:
    """The WebSocket fallback, raced against QUIC for ``http(s)`` URLs.

    ``enabled=False`` stops the fallback, for a relay that only serves QUIC, so a failed
    QUIC dial reports its own error. ``delay`` is the head start QUIC gets before the
    fallback joins the race; ``None`` uses 200 ms, and zero races both at once.
    """

    enabled: bool | None = None
    delay: timedelta | None = None

    def _ffi(self) -> MoqWebSocketConfig:
        return MoqWebSocketConfig(enabled=self.enabled, delay=self.delay)


@dataclass(frozen=True)
class ConnectionStats:
    """Transport metrics for a session; each field is ``None`` when unreported."""

    rtt: timedelta | None
    estimated_send_rate_bps: int | None
    bytes_sent: int | None
    bytes_received: int | None
    bytes_lost: int | None
    packets_sent: int | None
    packets_received: int | None
    packets_lost: int | None
    version: str | None = None
    """The negotiated protocol version, spelled like ``Client(versions=...)``."""

    @staticmethod
    def _from_ffi(stats: MoqConnectionStats) -> ConnectionStats:
        return ConnectionStats(
            rtt=stats.rtt,
            estimated_send_rate_bps=stats.estimated_send_rate_bps,
            bytes_sent=stats.bytes_sent,
            bytes_received=stats.bytes_received,
            bytes_lost=stats.bytes_lost,
            packets_sent=stats.packets_sent,
            packets_received=stats.packets_received,
            packets_lost=stats.packets_lost,
            version=stats.version,
        )


@dataclass(frozen=True)
class Grant:
    """What the peer lets this side do, in this side's own paths.

    An empty pattern list grants nothing in that direction; ``**`` grants everything.
    """

    publish: tuple[str, ...]
    """The broadcast patterns this side may publish to the peer."""
    subscribe: tuple[str, ...]
    """The broadcast patterns this side may subscribe to from the peer."""
    expires: timedelta | None = None
    """How long until the grant lapses, measured when it was read, or ``None`` for never."""

    @staticmethod
    def _from_ffi(grant: MoqGrant | None) -> Grant | None:
        if grant is None:
            return None
        return Grant(
            publish=tuple(grant.publish),
            subscribe=tuple(grant.subscribe),
            expires=grant.expires,
        )

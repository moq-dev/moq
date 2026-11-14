"""Root records the wrapper owns, as frozen dataclasses with Python defaults.

Media timestamps cross as exact :class:`Timestamp` records; durations cross as
``timedelta``. ``ClientTls`` lives here with the rest of the client
config, so a bare string passed for a list is refused instead of splitting into
characters.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import timedelta
from importlib import metadata
from typing import ClassVar, cast

from moq_ffi import (
    MoqAnchor,
    MoqBackoff,
    MoqClientTls,
    MoqCompression,
    MoqCongestionControl,
    MoqConnectionStats,
    MoqDatagram,
    MoqDelay,
    MoqFrame,
    MoqGrant,
    MoqOrder,
    MoqQuicConfig,
    MoqRange,
    MoqSubscription,
    MoqTimestamp,
    MoqTrackInfo,
    MoqVideoDecodedFrame,
    MoqVideoPixelFormat,
    MoqVideoSurface,
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


def _strs(value: Sequence[str], name: str) -> list[str]:
    # A str is itself a Sequence[str], so list() would split it into characters.
    if isinstance(value, str):
        raise TypeError(f"{name} takes a sequence of strings, not a str: {value!r}")
    return list(value)


@dataclass(frozen=True)
class Timestamp:
    """An exact media timestamp: ``value`` units at ``scale`` units per second.

    ``value`` is at most ``2**61 - 1`` and ``scale`` is non-zero and at most ``2**62 - 1``;
    a write past either bound raises ``moq.Error.TimeOverflow``. A read carries the
    track's timescale, and a write at another scale is floored to it.

    A scale-less track has no time, so there ``scale`` is ``None`` and ``value`` is the
    frame's group key (a datagram's sequence); build one with :meth:`unscaled`.
    """

    value: int
    scale: int | None

    @staticmethod
    def unscaled(key: int) -> Timestamp:
        """The scale-less timestamp at group key (or datagram sequence) ``key``."""
        return Timestamp(key, None)

    @staticmethod
    def from_timedelta(duration: timedelta) -> Timestamp:
        """Exactly ``duration``, at microsecond scale since that is a ``timedelta``'s own unit."""
        if duration < timedelta(0):
            raise ValueError(f"timestamp must not be negative: {duration}")
        return Timestamp(duration // _MICROSECOND, 1_000_000)

    def to_timedelta(self) -> timedelta:
        """This timestamp as a ``timedelta``, rounded down to a whole microsecond.

        The timestamp itself is exact; keep it, not the ``timedelta``, to republish a tick such as 90 kHz.
        Raises ``ValueError`` on an :meth:`unscaled` timestamp, which has no time.
        """
        if self.scale is None:
            raise ValueError(f"an unscaled timestamp (key {self.value}) has no time")
        return timedelta(microseconds=self.value * 1_000_000 // self.scale)

    def _ffi(self) -> MoqTimestamp:
        return MoqTimestamp(value=self.value, scale=self.scale)

    @staticmethod
    def _from_ffi(value: MoqTimestamp) -> Timestamp:
        return Timestamp(value.value, value.scale)


@dataclass(frozen=True)
class Frame:
    """A raw track frame: a payload and its presentation timestamp.

    On a scale-less track ``timestamp`` is :meth:`Timestamp.unscaled` at the frame's
    group key, on both publish and receive.
    """

    payload: bytes
    timestamp: Timestamp

    def _ffi(self) -> MoqFrame:
        return MoqFrame(payload=self.payload, timestamp=self.timestamp._ffi())

    @staticmethod
    def _from_ffi(frame: MoqFrame) -> Frame:
        return Frame(payload=frame.payload, timestamp=Timestamp._from_ffi(frame.timestamp))


@dataclass(frozen=True)
class Datagram:
    """A datagram payload with its own sequence and timestamp.

    On a scale-less track ``timestamp`` is :meth:`Timestamp.unscaled` at ``sequence``.
    """

    sequence: int
    timestamp: Timestamp
    payload: bytes

    def _ffi(self) -> MoqDatagram:
        return MoqDatagram(sequence=self.sequence, timestamp=self.timestamp._ffi(), payload=self.payload)

    @staticmethod
    def _from_ffi(datagram: MoqDatagram) -> Datagram:
        return Datagram(
            sequence=datagram.sequence,
            timestamp=Timestamp._from_ffi(datagram.timestamp),
            payload=datagram.payload,
        )


class Anchor:
    """Where one end of a :class:`Range` falls, mirroring moq-net's ``track::Anchor``.

    Build one with ``Anchor.OLDEST()``, ``Anchor.ABSOLUTE(group=...)``,
    ``Anchor.RELATIVE(groups=...)``, or ``Anchor.TIME(timestamp=...)``.
    """

    OLDEST: ClassVar[type[_AnchorOldest]]
    ABSOLUTE: ClassVar[type[_AnchorAbsolute]]
    RELATIVE: ClassVar[type[_AnchorRelative]]
    TIME: ClassVar[type[_AnchorTime]]

    def _ffi(self) -> MoqAnchor:
        raise TypeError("build an Anchor with Anchor.OLDEST, Anchor.ABSOLUTE, Anchor.RELATIVE, or Anchor.TIME")


# The generated variants subclass MoqAnchor only at runtime, so each _ffi casts for pyright.
@dataclass(frozen=True)
class _AnchorOldest(Anchor):
    """The oldest group still around, as far back as the max delay allows. A start only."""

    def _ffi(self) -> MoqAnchor:
        return cast(MoqAnchor, MoqAnchor.OLDEST())


@dataclass(frozen=True)
class _AnchorAbsolute(Anchor):
    """This group, delivered from its first frame."""

    group: int

    def _ffi(self) -> MoqAnchor:
        return cast(MoqAnchor, MoqAnchor.ABSOLUTE(group=self.group))


@dataclass(frozen=True)
class _AnchorRelative(Anchor):
    """The group this many before the latest, clamped to group 0: ``0`` is the latest group."""

    groups: int

    def _ffi(self) -> MoqAnchor:
        return cast(MoqAnchor, MoqAnchor.RELATIVE(groups=self.groups))


@dataclass(frozen=True)
class _AnchorTime(Anchor):
    """The group containing this presentation time. An untimed track refuses it."""

    timestamp: Timestamp

    def _ffi(self) -> MoqAnchor:
        return cast(MoqAnchor, MoqAnchor.TIME(timestamp=self.timestamp._ffi()))


Anchor.OLDEST = _AnchorOldest
Anchor.ABSOLUTE = _AnchorAbsolute
Anchor.RELATIVE = _AnchorRelative
Anchor.TIME = _AnchorTime


@dataclass(frozen=True)
class Range:
    """A span of a track to deliver, from ``start`` to ``end``, ``None`` to keep following the live edge.

    A relative or time start is the first frame of its group, and a relative or time end
    includes its whole group. An absolute end is exclusive.
    """

    start: Anchor
    end: Anchor | None = None

    def _ffi(self) -> MoqRange:
        return MoqRange(start=self.start._ffi(), end=None if self.end is None else self.end._ffi())


@dataclass(frozen=True)
class Delay:
    """How far behind the live edge a subscriber or decoder plays, mirroring moq-net's ``track::Delay``.

    ``min`` is the target latency: the playout or jitter-buffer delay held behind the
    live edge, and where a cold join starts. ``None`` leaves it unstated, which a decoder
    plays as zero while the subscription keeps it unstated, so it never pulls the
    aggregate across subscribers to zero. A ``min`` past ``max`` starts at ``max``.
    ``max`` is how far a non-latest group may fall behind before it is skipped; zero
    skips at once, ``None`` sets no subscriber limit and leaves the publisher's max age
    alone to bound it, and it caps every range.

    A bare ``Delay()`` is moq-net's default, zero for both.
    """

    min: timedelta | None = timedelta(0)
    max: timedelta | None = timedelta(0)

    @classmethod
    def _from_ffi(cls, delay: MoqDelay) -> Delay:
        return cls(min=delay.min, max=delay.max)

    def _ffi(self) -> MoqDelay:
        return MoqDelay(min=self.min, max=self.max)


@dataclass(frozen=True)
class Subscription:
    """Subscriber-side delivery preferences, mirroring moq-net's ``track::Subscription``.

    ``priority`` is in ``-128..=127``, higher first, and defaults to 0, the midpoint.
    ``delay`` is where a cold join starts and how far a non-latest group may fall behind
    before it is skipped. ``ranges`` are the spans of the track to deliver, ``None`` for
    everything ``delay.max`` still allows. ``order`` is which pending group the publisher
    sends first, ``None`` for newest first.

    A bare ``Subscription()`` has moq-net's defaults, as :meth:`default` does.
    """

    priority: int = 0
    delay: Delay = Delay()
    ranges: Sequence[Range] | None = None
    order: MoqOrder | None = None

    @classmethod
    def default(cls) -> Subscription:
        """moq-net's defaults, whose zero ``delay`` starts at the newest group and skips a superseded one at once.

        A ``None`` subscription means the same; ``dataclasses.replace`` this one to change
        a field.
        """
        defaults = moq_subscription_defaults()
        return cls(
            priority=defaults.priority,
            delay=Delay._from_ffi(defaults.delay),
            # The FFI defaults never restrict ranges, so there is no record to convert.
            ranges=None,
            order=defaults.order,
        )

    def _ffi(self) -> MoqSubscription:
        return MoqSubscription(
            priority=self.priority,
            delay=self.delay._ffi(),
            ranges=None if self.ranges is None else [r._ffi() for r in self.ranges],
            order=self.order,
        )


def _subscription(subscription: Subscription | None) -> MoqSubscription | None:
    return None if subscription is None else subscription._ffi()


@dataclass(frozen=True)
class TrackInfo:
    """Publisher-side track properties, mirroring moq-net's ``track::Info``.

    ``priority`` is in ``-128..=127``, higher first, and defaults to 0, the midpoint. It breaks
    ties between subscriptions of equal subscriber priority. ``max_age`` is how long a
    superseded group stays fresh: the default zero serves only the live edge, and
    ``None`` sets no publisher limit, leaving only the cache to bound it. ``refresh`` is
    how many times its first frame a group's later bytes may grow before a joining
    subscriber asks for a new group, ``None`` to never ask. ``timescale`` is ticks per second:
    ``None`` means the track is untimed, on both publish and receive.
    ``compression`` compresses each group on the wire to peers that support it, ``None``
    sending frames as written; readers always see the original frames.
    """

    priority: int = 0
    max_age: timedelta | None = timedelta(0)
    refresh: int | None = None
    timescale: int | None = None
    compression: MoqCompression | None = None

    def _ffi(self) -> MoqTrackInfo:
        return MoqTrackInfo(
            priority=self.priority,
            max_age=self.max_age,
            max_age_unlimited=self.max_age is None,
            refresh=self.refresh,
            timescale=self.timescale,
            compression=self.compression,
        )

    @staticmethod
    def _from_ffi(info: MoqTrackInfo) -> TrackInfo:
        return TrackInfo(
            priority=info.priority,
            max_age=None if info.max_age_unlimited else (info.max_age or timedelta(0)),
            refresh=info.refresh,
            timescale=info.timescale,
            compression=info.compression,
        )


def _track_info(info: TrackInfo | None) -> MoqTrackInfo | None:
    return None if info is None else info._ffi()


@dataclass(frozen=True)
class Backoff:
    """Retry pacing for the automatic reconnect.

    It applies only once a session has been established: a first dial that reaches no
    session ends the connection with its error rather than retrying.

    The delay starts at ``initial``, multiplies by ``multiplier`` after each failed
    attempt, and caps at ``max``. After ``timeout`` of consecutive failures the
    connection gives up; ``timedelta(0)`` retries forever. ``None`` keeps each
    default: 1s, x2, 5s, and no give-up.
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
    the ping interval (zero uses the default). ``gso``, ``mtu_discovery``, and
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
    datagrams_unrouted: int | None = None
    """Datagrams received for a subscription with no datagram track; ``None`` on a version without datagrams."""

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
            datagrams_unrouted=stats.datagrams_unrouted,
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


class VideoDecodedFrame:
    """One decoded picture. It owns the decoder's buffer until garbage collected, so drop it promptly."""

    def __init__(self, inner: MoqVideoDecodedFrame) -> None:
        self._inner = inner

    def timestamp(self) -> Timestamp:
        """The presentation timestamp, at the track's timescale."""
        return Timestamp._from_ffi(self._inner.timestamp())

    def width(self) -> int:
        """The picture's width in pixels."""
        return self._inner.width()

    def height(self) -> int:
        """The picture's height in pixels."""
        return self._inner.height()

    def pixels(self, format: MoqVideoPixelFormat) -> bytes:
        """The picture converted to tightly packed CPU pixels in ``format``."""
        return self._inner.pixels(format)

    def surface(self) -> VideoSurface | None:
        """The decoder's platform surface, when the decoder output asked for one."""
        return _surface(self._inner.surface())


class VideoSurface:
    """A decoded frame's platform surface; match it with a fallback arm, since new platforms add kinds."""


@dataclass(frozen=True)
class VideoSurfacePixelBuffer(VideoSurface):
    """An Apple ``CVPixelBufferRef`` (IOSurface-backed NV12) address, valid while its frame lives."""

    pointer: int


@dataclass(frozen=True)
class VideoSurfaceUnknown(VideoSurface):
    """A surface kind this version of the package does not know yet."""


def _surface(surface: MoqVideoSurface | None) -> VideoSurface | None:
    if surface is None:
        return None
    if isinstance(surface, MoqVideoSurface.PIXEL_BUFFER):
        return VideoSurfacePixelBuffer(surface.pointer)
    return VideoSurfaceUnknown()

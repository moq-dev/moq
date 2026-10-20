"""Root records that carry durations, owned here so they read as ``timedelta``.

moq-ffi spells every duration as integer microseconds, because not every target
language has a duration type. Python does, so these mirror the generated records
with ``timedelta`` fields and convert at the boundary.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import timedelta

from moq_ffi import (
    MoqBackoff,
    MoqConnectionStats,
    MoqDatagram,
    MoqFrame,
    MoqOrder,
    MoqRange,
    MoqSubscription,
    MoqTrackInfo,
)

_MICROSECOND = timedelta(microseconds=1)


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

    ``max_delay`` is how far a non-latest group may fall behind before it is skipped;
    zero skips at once, and it caps every range. ``ranges`` are the spans of the track to
    deliver, ``None`` for everything ``max_delay`` still allows. ``order`` is which pending
    group the publisher sends first, ``None`` for newest first.
    """

    priority: int = 0
    max_delay: timedelta = timedelta(0)
    ranges: Sequence[MoqRange] | None = None
    order: MoqOrder | None = None

    def _ffi(self) -> MoqSubscription:
        return MoqSubscription(
            priority=self.priority,
            max_delay_us=_to_us(self.max_delay, "max_delay"),
            ranges=None if self.ranges is None else list(self.ranges),
            order=self.order,
        )


def _subscription(subscription: Subscription | None) -> MoqSubscription | None:
    return None if subscription is None else subscription._ffi()


@dataclass(frozen=True)
class TrackInfo:
    """Publisher-side track properties, mirroring moq-net's ``track::Info``.

    ``priority`` defaults to 127, the middle of the range. ``max_age`` is how long the
    publisher caches a non-latest group, ``None`` for no limit. ``refresh`` is how many
    times its first frame a group's later bytes may grow before a joining subscriber
    asks for a new group, ``None`` to never ask. ``timescale`` is ticks per second:
    ``None`` means the track is untimed, on both publish and receive.
    """

    priority: int = 127
    max_age: timedelta | None = None
    refresh: int | None = None
    timescale: int | None = None

    def _ffi(self) -> MoqTrackInfo:
        return MoqTrackInfo(
            priority=self.priority,
            max_age_us=_opt_us(self.max_age, "max_age"),
            refresh=self.refresh,
            timescale=self.timescale,
        )

    @staticmethod
    def _from_ffi(info: MoqTrackInfo) -> TrackInfo:
        return TrackInfo(
            priority=info.priority,
            max_age=None if info.max_age_us is None else _from_us(info.max_age_us),
            refresh=info.refresh,
            timescale=info.timescale,
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
            initial_us=_opt_us(self.initial, "initial"),
            multiplier=self.multiplier,
            max_us=_opt_us(self.max, "max"),
            timeout_us=_opt_us(self.timeout, "timeout"),
        )


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
            rtt=None if stats.rtt_us is None else _from_us(stats.rtt_us),
            estimated_send_rate_bps=stats.estimated_send_rate_bps,
            bytes_sent=stats.bytes_sent,
            bytes_received=stats.bytes_received,
            bytes_lost=stats.bytes_lost,
            packets_sent=stats.packets_sent,
            packets_received=stats.packets_received,
            packets_lost=stats.packets_lost,
            version=stats.version,
        )

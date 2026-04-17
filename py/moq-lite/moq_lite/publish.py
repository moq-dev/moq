"""Producer wrappers — publish broadcasts and media tracks."""

from __future__ import annotations

from typing import TYPE_CHECKING

from moq_ffi import MoqBroadcastProducer, MoqMediaProducer, MoqRawGroupProducer, MoqRawProducer

if TYPE_CHECKING:
    from .subscribe import BroadcastConsumer, RawConsumer, RawGroupConsumer


class MediaProducer:
    """Wraps MoqMediaProducer with a cleaner interface."""

    def __init__(self, inner: MoqMediaProducer) -> None:
        self._inner = inner

    def write_frame(self, payload: bytes, timestamp_us: int) -> None:
        self._inner.write_frame(payload, timestamp_us)

    def finish(self) -> None:
        self._inner.finish()


class RawGroupProducer:
    """Writes frames into a single group on a raw track."""

    def __init__(self, inner: MoqRawGroupProducer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    def consume(self) -> RawGroupConsumer:
        """Create a consumer that reads frames from this group."""
        from .subscribe import RawGroupConsumer

        return RawGroupConsumer(self._inner.consume())

    def write_frame(self, payload: bytes) -> None:
        self._inner.write_frame(payload)

    def finish(self) -> None:
        self._inner.finish()


class RawProducer:
    """Raw track producer — write arbitrary byte payloads with no codec required.

    Same pattern as moq-boy's status/command tracks.
    """

    def __init__(self, inner: MoqRawProducer) -> None:
        self._inner = inner

    def append_group(self) -> RawGroupProducer:
        """Start a new group; write frames into it, then finish()."""
        return RawGroupProducer(self._inner.append_group())

    def write_frame(self, payload: bytes) -> None:
        """Convenience: write a single-frame group in one call."""
        self._inner.write_frame(payload)

    def consume(self) -> RawConsumer:
        """Create a consumer that reads directly from this producer's track."""
        from .subscribe import RawConsumer

        return RawConsumer(self._inner.consume())

    def finish(self) -> None:
        self._inner.finish()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface."""

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    def publish_media(self, format: str, init: bytes) -> MediaProducer:
        return MediaProducer(self._inner.publish_media(format, init))

    def publish_raw(self, name: str) -> RawProducer:
        """Create a raw track — send any bytes, no codec validation."""
        return RawProducer(self._inner.publish_raw(name))

    def consume(self) -> BroadcastConsumer:
        """Create a consumer that reads from this broadcast's tracks."""
        from .subscribe import BroadcastConsumer

        return BroadcastConsumer(self._inner.consume())

    def finish(self) -> None:
        self._inner.finish()

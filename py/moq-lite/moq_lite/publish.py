"""Producer wrappers — publish broadcasts and media tracks."""

from __future__ import annotations

from moq_ffi import MoqBroadcastProducer, MoqMediaProducer, MoqRawProducer


class MediaProducer:
    """Wraps MoqMediaProducer with a cleaner interface."""

    def __init__(self, inner: MoqMediaProducer) -> None:
        self._inner = inner

    def write_frame(self, payload: bytes, timestamp_us: int) -> None:
        self._inner.write_frame(payload, timestamp_us)

    def finish(self) -> None:
        self._inner.finish()


class RawProducer:
    """Raw track producer — write arbitrary byte payloads with no codec required.

    Same pattern as moq-boy's status/command tracks.
    """

    def __init__(self, inner: MoqRawProducer) -> None:
        self._inner = inner

    def write_frame(self, payload: bytes) -> None:
        self._inner.write_frame(payload)

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

    def finish(self) -> None:
        self._inner.finish()

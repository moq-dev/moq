"""Publish opaque payload tracks, mirroring the ``moq-flate`` crate."""

from __future__ import annotations

from datetime import timedelta

from moq_ffi import MoqFlateConfig, MoqFlateSnapshotProducer, MoqFlateStreamProducer

from . import publish
from ._records import _opt_us

__all__ = ["SnapshotProducer", "StreamProducer"]


class SnapshotProducer:
    """Publish the latest opaque payload, advertised in the broadcast catalog."""

    def __init__(
        self,
        broadcast: publish.BroadcastProducer,
        track: publish.TrackProducer,
        *,
        compression: bool = False,
        mime: str | None = None,
    ) -> None:
        self._inner = MoqFlateSnapshotProducer(
            broadcast._inner, track._inner, MoqFlateConfig(compression=compression, mime=mime)
        )

    def update(self, payload: bytes, *, timestamp: timedelta | None = None) -> None:
        """Replace the payload at the given broadcast time, or leave it untimed."""
        self._inner.update(payload, _opt_us(timestamp, "timestamp"))

    def finish(self) -> None:
        """Finish the track and retire its catalog entry."""
        self._inner.finish()


class StreamProducer:
    """Publish an ordered opaque payload log, advertised in the broadcast catalog."""

    def __init__(
        self,
        broadcast: publish.BroadcastProducer,
        track: publish.TrackProducer,
        *,
        compression: bool = False,
        mime: str | None = None,
    ) -> None:
        self._inner = MoqFlateStreamProducer(
            broadcast._inner, track._inner, MoqFlateConfig(compression=compression, mime=mime)
        )

    def append(self, payload: bytes, *, timestamp: timedelta | None = None) -> None:
        """Append a payload at the given broadcast time, or leave it untimed."""
        self._inner.append(payload, _opt_us(timestamp, "timestamp"))

    def finish(self) -> None:
        """Finish the track and retire its catalog entry."""
        self._inner.finish()

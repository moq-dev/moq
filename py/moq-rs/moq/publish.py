"""Producer wrappers — publish broadcasts and media tracks."""

from __future__ import annotations

from typing import TYPE_CHECKING

from ._uniffi import (
    MoqAudioProducer,
    MoqBroadcastProducer,
    MoqGroupProducer,
    MoqMediaProducer,
    MoqTrackProducer,
)
from .types import AudioEncoderInput, AudioEncoderOutput, AudioFrame

if TYPE_CHECKING:
    from .subscribe import BroadcastConsumer, GroupConsumer, TrackConsumer


class MediaProducer:
    """Wraps MoqMediaProducer with a cleaner interface."""

    def __init__(self, inner: MoqMediaProducer) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The generated media track name."""
        return self._inner.name()

    async def used(self) -> None:
        """Wait until this media track has at least one active subscriber."""
        await self._inner.used()

    async def unused(self) -> None:
        """Wait until this media track has no active subscribers."""
        await self._inner.unused()

    def write_frame(self, payload: bytes, timestamp_us: int) -> None:
        self._inner.write_frame(payload, timestamp_us)

    def finish(self) -> None:
        self._inner.finish()


class GroupProducer:
    """Writes frames into a single group on a track."""

    def __init__(self, inner: MoqGroupProducer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    def consume(self) -> GroupConsumer:
        """Create a consumer that reads frames from this group."""
        from .subscribe import GroupConsumer

        return GroupConsumer(self._inner.consume())

    def write_frame(self, payload: bytes) -> None:
        self._inner.write_frame(payload)

    def finish(self) -> None:
        self._inner.finish()


class TrackProducer:
    """Track producer — write arbitrary byte payloads with no codec required.

    Same pattern as moq-boy's status/command tracks.
    """

    def __init__(self, inner: MoqTrackProducer) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The track name."""
        return self._inner.name()

    async def used(self) -> None:
        """Wait until this track has at least one active subscriber."""
        await self._inner.used()

    async def unused(self) -> None:
        """Wait until this track has no active subscribers."""
        await self._inner.unused()

    def append_group(self) -> GroupProducer:
        """Start a new group; write frames into it, then finish()."""
        return GroupProducer(self._inner.append_group())

    def write_frame(self, payload: bytes) -> None:
        """Convenience: write a single-frame group in one call."""
        self._inner.write_frame(payload)

    def consume(self) -> TrackConsumer:
        """Create a consumer that reads directly from this producer's track."""
        from .subscribe import TrackConsumer

        return TrackConsumer(self._inner.consume())

    def finish(self) -> None:
        self._inner.finish()


class AudioProducer:
    """Publish raw PCM and let libopus encode it on the way out.

    Built via :meth:`BroadcastProducer.publish_audio`. PCM layout
    (format / sample rate / channels / bitrate / frame duration) is
    fixed at construction; each :meth:`write` call passes only bytes
    and a presentation timestamp.
    """

    def __init__(self, inner: MoqAudioProducer) -> None:
        self._inner = inner

    def write(self, frame: AudioFrame) -> None:
        """Push one frame of PCM in the configured input format."""
        self._inner.write(frame)

    def finish(self) -> None:
        """Flush any pending samples and finalize the track."""
        self._inner.finish()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface."""

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    def publish_media(self, format: str, init: bytes) -> MediaProducer:
        return MediaProducer(self._inner.publish_media(format, init))

    def publish_audio(
        self,
        name: str,
        input: AudioEncoderInput,
        output: AudioEncoderOutput,
    ) -> AudioProducer:
        """Publish a raw-audio track with an in-process Opus encoder."""
        return AudioProducer(self._inner.publish_audio(name, input, output))

    def publish_track(self, name: str) -> TrackProducer:
        """Create a track — send any bytes, no codec validation."""
        return TrackProducer(self._inner.publish_track(name))

    def consume(self) -> BroadcastConsumer:
        """Create a consumer that reads from this broadcast's tracks."""
        from .subscribe import BroadcastConsumer

        return BroadcastConsumer(self._inner.consume())

    def finish(self) -> None:
        self._inner.finish()

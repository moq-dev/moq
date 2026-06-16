"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

from typing import TYPE_CHECKING

from moq_ffi import (
    MoqAudioProducer,
    MoqBroadcastDynamic,
    MoqBroadcastProducer,
    MoqGroupProducer,
    MoqMediaProducer,
    MoqMediaStreamProducer,
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


class MediaStreamProducer:
    """Wraps MoqMediaStreamProducer: feed a raw byte stream (e.g. Annex-B
    H.264) and let the importer infer frame boundaries.

    Built via :meth:`BroadcastProducer.publish_media_stream`. Unlike
    :class:`MediaProducer`, no per-frame timestamps are needed; just push
    encoder bytes as they arrive.
    """

    def __init__(self, inner: MoqMediaStreamProducer) -> None:
        self._inner = inner

    def write(self, payload: bytes) -> None:
        """Push raw stream bytes; whole frames are emitted as they complete."""
        self._inner.write(payload)

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
    """Track producer: write arbitrary byte payloads with no codec required.

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

    def abort(self, error_code: int) -> None:
        """Abort this track with an application error code."""
        self._inner.abort(error_code)

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


class BroadcastDynamic:
    """Async source of tracks requested by subscribers.

    Hold this object while subscriptions to unknown tracks should be accepted.
    """

    def __init__(self, inner: MoqBroadcastDynamic) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> TrackProducer:
        return await self.requested_track()

    async def requested_track(self) -> TrackProducer:
        return TrackProducer(await self._inner.requested_track())

    def cancel(self) -> None:
        self._inner.cancel()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface."""

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    def dynamic(self) -> BroadcastDynamic:
        """Accept subscriptions to tracks that are not published yet."""
        return BroadcastDynamic(self._inner.dynamic())

    def publish_media(self, format: str, init: bytes) -> MediaProducer:
        return MediaProducer(self._inner.publish_media(format, init))

    def publish_media_on_track(self, track: TrackProducer, format: str, init: bytes) -> MediaProducer:
        return MediaProducer(self._inner.publish_media_on_track(track._inner, format, init))

    def publish_media_stream(self, format: str) -> MediaStreamProducer:
        """Publish a media track fed by a raw byte stream (unknown frame
        boundaries). `format` is a stream format (avc3, hev1, av01, fmp4, mkv)."""
        return MediaStreamProducer(self._inner.publish_media_stream(format))

    def publish_audio(
        self,
        name: str,
        input: AudioEncoderInput,
        output: AudioEncoderOutput,
    ) -> AudioProducer:
        """Publish a raw-audio track with an in-process Opus encoder."""
        return AudioProducer(self._inner.publish_audio(name, input, output))

    def publish_track(self, name: str) -> TrackProducer:
        """Create a track. Send any bytes, no codec validation."""
        return TrackProducer(self._inner.publish_track(name))

    def consume(self) -> BroadcastConsumer:
        """Create a consumer that reads from this broadcast's tracks."""
        from .subscribe import BroadcastConsumer

        return BroadcastConsumer(self._inner.consume())

    def finish(self) -> None:
        self._inner.finish()

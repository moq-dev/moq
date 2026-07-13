"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any

from moq_ffi import (
    MoqAudioProducer,
    MoqBroadcastDynamic,
    MoqBroadcastProducer,
    MoqGroupProducer,
    MoqGroupRequest,
    MoqInit,
    MoqJsonConfig,
    MoqJsonProducer,
    MoqJsonStreamConfig,
    MoqJsonStreamProducer,
    MoqMediaProducer,
    MoqMediaStreamProducer,
    MoqTrackDynamic,
    MoqTrackProducer,
    MoqTrackRequest,
)

from .types import AudioEncoderInput, AudioEncoderOutput, AudioFrame, Subscription, TrackInfo, VideoHint

if TYPE_CHECKING:
    from .subscribe import BroadcastConsumer, GroupConsumer, TrackConsumer


def _media_init(format: str, init: bytes, video: VideoHint | None) -> MoqInit:
    return MoqInit(format=format, data=init, video=video)


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

    def write_frame(self, payload: bytes, timestamp_us: int) -> None:
        """Write a frame with a presentation timestamp in microseconds."""
        self._inner.write_frame(payload, timestamp_us)

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

    def dynamic(self) -> TrackDynamic:
        """Serve fetches for groups that are not currently cached."""
        return TrackDynamic(self._inner.dynamic())

    def append_group(self) -> GroupProducer:
        """Start a new group; write frames into it, then finish()."""
        return GroupProducer(self._inner.append_group())

    def write_frame(self, payload: bytes, timestamp_us: int) -> None:
        """Write a single-frame group with a timestamp in microseconds."""
        self._inner.write_frame(payload, timestamp_us)

    def append_datagram(self, timestamp_us: int, payload: bytes) -> int:
        """Send a best-effort datagram and return its sequence number.

        Payloads are capped at 1200 bytes. Datagram delivery requires a datagram-capable
        transport and wire version; there is no stream fallback.
        """
        return self._inner.append_datagram(timestamp_us, payload)

    def consume(self, subscription: Subscription | None = None) -> TrackConsumer:
        """Create a consumer that reads directly from this producer's track.

        ``subscription`` tunes delivery priority, group ordering priority, and group range; omit for defaults.
        """
        from .subscribe import TrackConsumer

        return TrackConsumer(self._inner.consume(subscription))

    def abort(self, error_code: int) -> None:
        """Abort this track with an application error code."""
        self._inner.abort(error_code)

    def finish(self) -> None:
        self._inner.finish()


class TrackRequest:
    """A subscriber-requested track that hasn't been accepted yet.

    Accept it for raw writes, hand it to :meth:`BroadcastProducer.publish_media_on_track`
    to publish media (the importer accepts it), or abort it to reject the subscriber.
    """

    def __init__(self, inner: MoqTrackRequest) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The requested track name."""
        return self._inner.name()

    def accept(self, info: TrackInfo | None = None) -> TrackProducer:
        """Accept the request as a raw track.

        ``info`` fixes the track's timescale, priority, ordering priority, and cache; omit for defaults.
        """
        return TrackProducer(self._inner.accept(info))

    def dynamic(self) -> TrackDynamic:
        """Create a fetch handler before accepting this requested track."""
        return TrackDynamic(self._inner.dynamic())

    def abort(self, error_code: int) -> None:
        """Reject the request with an application error code."""
        self._inner.abort(error_code)


class GroupRequest:
    """A request to produce one uncached group for a fetch consumer."""

    def __init__(self, inner: MoqGroupRequest) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The requested group sequence within the track."""
        return self._inner.sequence()

    @property
    def priority(self) -> int:
        """The consumer's delivery priority for this fetch."""
        return self._inner.priority()

    def accept(self) -> GroupProducer:
        """Accept the request and return a producer for the group."""
        return GroupProducer(self._inner.accept())

    def abort(self, error_code: int) -> None:
        """Reject the fetch with an application error code."""
        self._inner.abort(error_code)


class TrackDynamic:
    """Async source of uncached group requests for one track."""

    def __init__(self, inner: MoqTrackDynamic) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> GroupRequest:
        return await self.requested_group()

    async def requested_group(self) -> GroupRequest:
        """Wait for the next uncached group request."""
        return GroupRequest(await self._inner.requested_group())

    def cancel(self) -> None:
        """Cancel current and future group request waits."""
        self._inner.cancel()


class JsonProducer:
    """Publish a JSON value that consumers see as a single latest state (lossy).

    Built via :meth:`BroadcastProducer.publish_json`. Each :meth:`update` supersedes the
    last; a late joiner only sees the newest value. Values are any JSON-serializable Python
    object, encoded as snapshots and merge-patch deltas automatically.
    """

    def __init__(self, inner: MoqJsonProducer) -> None:
        self._inner = inner

    def update(self, value: Any) -> None:
        """Publish a new value. A no-op if unchanged from the previous update."""
        self._inner.update(json.dumps(value))

    def finish(self) -> None:
        """Finish the track, closing any open group."""
        self._inner.finish()


class JsonStreamProducer:
    """Publish an ordered log of JSON records (lossless).

    Built via :meth:`BroadcastProducer.publish_json_stream`. Every :meth:`append` is
    preserved and delivered in order. Records are any JSON-serializable Python object.
    """

    def __init__(self, inner: MoqJsonStreamProducer) -> None:
        self._inner = inner

    def append(self, value: Any) -> None:
        """Append one record to the log."""
        self._inner.append(json.dumps(value))

    def finish(self) -> None:
        """Finish the track, closing the group."""
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

    async def __anext__(self) -> TrackRequest:
        return await self.requested_track()

    async def requested_track(self) -> TrackRequest:
        return TrackRequest(await self._inner.requested_track())

    def cancel(self) -> None:
        self._inner.cancel()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface."""

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    def dynamic(self) -> BroadcastDynamic:
        """Accept subscriptions to tracks that are not published yet."""
        return BroadcastDynamic(self._inner.dynamic())

    def publish_media(
        self,
        format: str,
        init: bytes = b"",
        video: VideoHint | None = None,
    ) -> MediaProducer:
        """Publish a single media track. `format` selects the codec (e.g. "opus", "avc3"); `init` is
        its codec init bytes (required for audio formats). `video` seeds catalog fields the stream
        can't reveal (bitrate) or publishes the catalog before the first keyframe. See
        :class:`VideoHint`."""
        return MediaProducer(self._inner.publish_media(_media_init(format, init, video)))

    def publish_media_on_track(
        self,
        request: TrackRequest,
        format: str,
        init: bytes = b"",
        video: VideoHint | None = None,
    ) -> MediaProducer:
        """Publish media onto a requested track. See :meth:`publish_media` for the arguments."""
        return MediaProducer(self._inner.publish_media_on_track(request._inner, _media_init(format, init, video)))

    def publish_media_stream(
        self,
        format: str,
        video: VideoHint | None = None,
    ) -> MediaStreamProducer:
        """Publish a media track fed by a raw byte stream (unknown frame
        boundaries). `format` is a stream format (avc3, hev1, av01, fmp4, mkv).
        `video` seeds catalog fields as in :meth:`publish_media`."""
        return MediaStreamProducer(self._inner.publish_media_stream(_media_init(format, b"", video)))

    def publish_audio(
        self,
        name: str,
        input: AudioEncoderInput,
        output: AudioEncoderOutput,
    ) -> AudioProducer:
        """Publish a raw-audio track with an in-process Opus encoder."""
        return AudioProducer(self._inner.publish_audio(name, input, output))

    def publish_track(self, name: str, info: TrackInfo | None = None) -> TrackProducer:
        """Create a track. Send any bytes, no codec validation. ``info`` sets track
        properties (priority, cache, timescale); omit for defaults."""
        return TrackProducer(self._inner.publish_track(name, info))

    def publish_json(self, name: str, *, delta_ratio: int = 8, compression: bool = False) -> JsonProducer:
        """Publish a JSON snapshot track (lossy latest-value).

        Each update supersedes the last; a late joiner only sees the newest value.
        ``delta_ratio`` controls how aggressively deltas are emitted instead of full
        snapshots (0 disables deltas). Set ``compression`` to DEFLATE-compress each group;
        the consumer must pass the same flag. Advertise the track with
        :meth:`set_catalog_section` if consumers should discover it.
        """
        config = MoqJsonConfig(delta_ratio=delta_ratio, compression=compression)
        return JsonProducer(self._inner.publish_json(name, config))

    def publish_json_stream(self, name: str, *, compression: bool = False) -> JsonStreamProducer:
        """Publish a JSON stream track (lossless append-log).

        Every appended record is preserved and delivered in order. Set ``compression`` to
        DEFLATE-compress the group; the consumer must pass the same flag.
        """
        config = MoqJsonStreamConfig(compression=compression)
        return JsonStreamProducer(self._inner.publish_json_stream(name, config))

    def set_catalog_section(self, name: str, value: str) -> None:
        """Set or replace an untyped application section in the catalog.

        `value` is a JSON string that lands as a top-level catalog key alongside
        `video`/`audio` and reaches subscribers via `Catalog.sections`. `name` must not
        be a reserved media section ("video"/"audio"). The catalog is republished
        automatically. Use this to advertise a side-channel track (e.g. a transcript
        or captions track) that the catalog doesn't model natively.
        """
        self._inner.set_catalog_section(name, value)

    def remove_catalog_section(self, name: str) -> None:
        """Remove an untyped application section from the catalog by name.

        A no-op if no section with that name exists. The catalog is republished
        automatically.
        """
        self._inner.remove_catalog_section(name)

    def consume(self) -> BroadcastConsumer:
        """Create a consumer that reads from this broadcast's tracks."""
        from .subscribe import BroadcastConsumer

        return BroadcastConsumer(self._inner.consume())

    def finish(self) -> None:
        self._inner.finish()

"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any

from moq_ffi import (
    MoqAudioFormat,
    MoqAudioInit,
    MoqAudioProducer,
    MoqBroadcastDynamic,
    MoqBroadcastProducer,
    MoqContainerFormat,
    MoqContainerInit,
    MoqContainerProducer,
    MoqContainerStreamProducer,
    MoqGroupProducer,
    MoqGroupRequest,
    MoqJsonSnapshotConfig,
    MoqJsonSnapshotProducer,
    MoqJsonStreamConfig,
    MoqJsonStreamProducer,
    MoqMediaProducer,
    MoqMediaStreamProducer,
    MoqTrackDynamic,
    MoqTrackProducer,
    MoqTrackRequest,
    MoqVideoFormat,
    MoqVideoInit,
    MoqVideoProducer,
)

from .types import (
    AudioEncoderInput,
    AudioEncoderOutput,
    AudioFrame,
    Frame,
    Route,
    Subscription,
    TrackInfo,
    VideoEncoderInput,
    VideoEncoderOutput,
    VideoFrame,
    VideoHint,
    VideoProperties,
)

if TYPE_CHECKING:
    from .subscribe import BroadcastConsumer, GroupConsumer, TrackConsumer


def _audio_init(format: MoqAudioFormat, init: bytes, label: str | None) -> MoqAudioInit:
    return MoqAudioInit(format=format, data=init, label=label)


def _video_init(format: MoqVideoFormat, init: bytes, label: str | None, hint: VideoHint | None) -> MoqVideoInit:
    return MoqVideoInit(format=format, data=init, label=label, hint=hint)


class MediaProducer:
    """Publish encoded media frames on a single track, one payload at a time.

    Built via :meth:`BroadcastProducer.publish_audio` or
    :meth:`BroadcastProducer.publish_video`. Push each encoded frame with
    :meth:`write_frame`, then :meth:`finish` when the stream ends.
    """

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

    def write_frame(self, payload: bytes, timestamp_us: int = 0) -> None:
        """Write one encoded frame with a presentation timestamp in microseconds."""
        self._inner.write_frame(Frame(payload=payload, timestamp_us=timestamp_us))

    def cut(self) -> None:
        """Draw a group boundary here.

        Audio has no boundary of its own (every packet is independently
        decodable), so this is the only thing that gives it groups: call it
        after every frame for one group (one QUIC stream) the relay forwards
        without waiting, or at a segment cadence to align with video. Video
        groups at its own keyframes and needs this only to override that.
        """
        self._inner.cut()

    def seek(self, sequence: int) -> None:
        """Draw a group boundary and number the next group ``sequence``.

        :meth:`cut` with an explicit sequence, for a publisher whose group
        numbers have to be deterministic: two encoders aligning per GOP so a
        consumer can fail over between them.
        """
        self._inner.seek(sequence)

    def finish(self) -> None:
        """Finish publishing and flush a clean end to subscribers."""
        self._inner.finish()


class ContainerProducer:
    """Publish a container, which demuxes and publishes its own tracks.

    Built via :meth:`BroadcastProducer.publish_container`. Unlike
    :class:`MediaProducer` there is no per-frame timestamp: a container carries
    its tracks' timing itself.
    """

    def __init__(self, inner: MoqContainerProducer) -> None:
        self._inner = inner

    def write(self, payload: bytes) -> None:
        """Write a whole chunk of container bytes."""
        self._inner.write(payload)

    def cut(self) -> None:
        """Declare that the next chunk starts a new segment, rolling a group on every track.

        An fMP4 source carrying ``styp`` atoms declares its own segments, so
        this is only needed when it doesn't. Formats with no segment concept
        (MKV, TS, FLV) ignore it.
        """
        self._inner.cut()

    def seek(self, sequence: int) -> None:
        """Start a new segment and number its groups ``sequence``."""
        self._inner.seek(sequence)

    def finish(self) -> None:
        """Finish every track this container publishes."""
        self._inner.finish()


class ContainerStreamProducer:
    """Publish a container fed by a raw byte stream, which recovers its own framing.

    Built via :meth:`BroadcastProducer.publish_container_stream`.
    """

    def __init__(self, inner: MoqContainerStreamProducer) -> None:
        self._inner = inner

    def write(self, payload: bytes) -> None:
        """Push raw container bytes; chunk boundaries don't matter."""
        self._inner.write(payload)

    def finish(self) -> None:
        """Finish every track this container publishes."""
        self._inner.finish()


class MediaStreamProducer:
    """Wraps MoqMediaStreamProducer: feed a raw byte stream (e.g. Annex-B
    H.264) and let the importer infer frame boundaries.

    Built via :meth:`BroadcastProducer.publish_video_stream`. Unlike
    :class:`MediaProducer`, no per-frame timestamps are needed; just push
    encoder bytes as they arrive.
    """

    def __init__(self, inner: MoqMediaStreamProducer) -> None:
        self._inner = inner

    def write(self, payload: bytes) -> None:
        """Push raw stream bytes; whole frames are emitted as they complete."""
        self._inner.write(payload)

    def finish(self) -> None:
        """Finish publishing and flush a clean end to subscribers."""
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

    def write_frame(self, payload: bytes, timestamp_us: int = 0) -> None:
        """Write a frame with a presentation timestamp in microseconds."""
        self._inner.write_frame(Frame(payload=payload, timestamp_us=timestamp_us))

    def finish(self) -> None:
        """Close this group cleanly, marking it complete for subscribers."""
        self._inner.finish()

    def abort(self, error_code: int) -> None:
        """Abort this group with an application error code."""
        self._inner.abort(error_code)


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

    def create_group(self, sequence: int) -> GroupProducer:
        """Create a group with an explicit sequence number."""
        return GroupProducer(self._inner.create_group(sequence))

    def write_frame(self, payload: bytes, timestamp_us: int = 0) -> None:
        """Write a single-frame group with a timestamp in microseconds."""
        self._inner.write_frame(Frame(payload=payload, timestamp_us=timestamp_us))

    def append_datagram(self, payload: bytes, timestamp_us: int = 0) -> int:
        """Send a best-effort datagram and return its sequence number.

        Payloads are capped at 1200 bytes. Datagram delivery requires a datagram-capable
        transport and wire version; there is no stream fallback.
        """
        return self._inner.append_datagram(Frame(payload=payload, timestamp_us=timestamp_us))

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
        """Finish publishing and flush a clean end to subscribers."""
        self._inner.finish()

    def finish_at(self, final_sequence: int) -> None:
        """Declare the exclusive final group sequence ahead of the live edge."""
        self._inner.finish_at(final_sequence)


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


class JsonSnapshotProducer:
    """Publish a JSON value that consumers see as a single latest state (lossy).

    Built via :meth:`BroadcastProducer.publish_json_snapshot`. Each :meth:`update` supersedes the
    last; a late joiner only sees the newest value. Values are any JSON-serializable Python
    object, encoded as snapshots and merge-patch deltas automatically.
    """

    def __init__(self, inner: MoqJsonSnapshotProducer) -> None:
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


class VideoProducer:
    """Publish raw pictures and let a native encoder compress them on the way out.

    Built via :meth:`BroadcastProducer.publish_video`. Pixel format,
    resolution, and framerate are fixed at construction; each
    :meth:`write` call passes only pixels and a presentation timestamp.
    """

    def __init__(self, inner: MoqVideoProducer) -> None:
        self._inner = inner

    def write(self, frame: VideoFrame) -> None:
        """Encode and publish one frame in the configured input format.

        A hardware encoder pipelines, so a call that puts nothing on the wire
        is normal rather than an error.
        """
        self._inner.write(frame)

    def cut(self) -> None:
        """Start a new group at the next written frame.

        Optional: the encoder keyframes every ``gop`` frames on its own, and
        each of those cuts a group, so a subscriber can always join without
        this. Reach for it only to place the boundaries yourself.
        """
        self._inner.cut()

    def set_bitrate(self, bitrate: int) -> None:
        """Retune the live encoder, in bits per second.

        Cheap enough to drive from a congestion controller: no keyframe is
        forced. Raises if this backend cannot retune while running. That is
        not fatal: the encoder keeps its current rate.
        """
        self._inner.set_bitrate(bitrate)

    def finish(self) -> None:
        """Flush any frames the codec is holding and finalize the track."""
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
        """Await the next track a subscriber requested but that isn't published yet."""
        return TrackRequest(await self._inner.requested_track())

    def cancel(self) -> None:
        """Stop accepting track requests and release the underlying handle."""
        self._inner.cancel()


class BroadcastProducer:
    """Wraps MoqBroadcastProducer with a cleaner interface.

    Constructing one directly creates a standalone broadcast for serving dynamic
    requests (:meth:`moq.BroadcastRequest.accept`) or local pub/sub. To publish at
    a path, use :meth:`moq.OriginProducer.create_broadcast` instead.
    """

    def __init__(self) -> None:
        self._inner = MoqBroadcastProducer()

    @classmethod
    def _from_inner(cls, inner: MoqBroadcastProducer) -> BroadcastProducer:
        """Wrap an existing FFI producer (e.g. one created from an origin)."""
        self = cls.__new__(cls)
        self._inner = inner
        return self

    def dynamic(self) -> BroadcastDynamic:
        """Accept subscriptions to tracks that are not published yet."""
        return BroadcastDynamic(self._inner.dynamic())

    def set_route(self, route: Route) -> None:
        """Update the broadcast's route: the hop chain, cost, and liveness it advertises.

        Use this as conditions shift (e.g. a standby transcoder lowering its
        ``cost`` once warm); consumers observe the change via
        :meth:`BroadcastConsumer.route_changed`.
        """
        self._inner.set_route(route)

    def set_announce(self, announce: bool) -> None:
        """Set whether the broadcast is announced, keeping the rest of its route.

        The origin advertises the path only while announced; an unannounced
        broadcast stays reachable by exact path for subscribes and fetches. This is
        how a publisher goes on and off the air without tearing down the broadcast.
        """
        self._inner.set_announce(announce)

    def set_video_properties(self, properties: VideoProperties) -> None:
        """Replace the catalog properties shared by every video rendition."""
        self._inner.set_video_properties(properties)

    def publish_audio(
        self,
        format: MoqAudioFormat,
        init: bytes,
        *,
        label: str | None = None,
    ) -> MediaProducer:
        """Publish one audio codec as a new track. `init` is required: audio resolves its whole
        rendition from those bytes (an OpusHead, an AudioSpecificConfig, a STREAMINFO). `label` is
        the human-readable rendition name stored in the catalog."""
        return MediaProducer(self._inner.publish_audio(_audio_init(format, init, label)))

    def publish_video(
        self,
        format: MoqVideoFormat,
        init: bytes = b"",
        *,
        label: str | None = None,
        hint: VideoHint | None = None,
    ) -> MediaProducer:
        """Publish one video codec as a new track. `init` may be empty for a format that resolves in
        band. `hint` seeds catalog fields the stream can't reveal (bitrate) or publishes the catalog
        before the first keyframe. See :class:`VideoHint`."""
        return MediaProducer(self._inner.publish_video(_video_init(format, init, label, hint)))

    def publish_container(
        self,
        format: MoqContainerFormat,
        init: bytes = b"",
    ) -> ContainerProducer:
        """Publish a container, which demuxes and publishes its own tracks. There is no label or
        hint: a container describes each track it publishes from its own metadata."""
        return ContainerProducer(self._inner.publish_container(MoqContainerInit(format=format, data=init)))

    def publish_audio_on_track(
        self,
        request: TrackRequest,
        format: MoqAudioFormat,
        init: bytes,
        *,
        label: str | None = None,
    ) -> MediaProducer:
        """Publish one audio codec onto a requested track. See :meth:`publish_audio`."""
        return MediaProducer(self._inner.publish_audio_on_track(request._inner, _audio_init(format, init, label)))

    def publish_video_on_track(
        self,
        request: TrackRequest,
        format: MoqVideoFormat,
        init: bytes = b"",
        *,
        label: str | None = None,
        hint: VideoHint | None = None,
    ) -> MediaProducer:
        """Publish one video codec onto a requested track. See :meth:`publish_video`."""
        return MediaProducer(self._inner.publish_video_on_track(request._inner, _video_init(format, init, label, hint)))

    def publish_video_stream(
        self,
        format: MoqVideoFormat,
        *,
        label: str | None = None,
        hint: VideoHint | None = None,
    ) -> MediaStreamProducer:
        """Publish a video track fed by a raw byte stream (unknown frame boundaries). Only the
        self-delimiting formats work: `AVC3`, `HEV1`, `AV01`. There is no audio counterpart, since
        audio has no frame boundaries to infer."""
        return MediaStreamProducer(self._inner.publish_video_stream(_video_init(format, b"", label, hint)))

    def publish_container_stream(self, format: MoqContainerFormat) -> ContainerStreamProducer:
        """Publish a container fed by a raw byte stream, which recovers its own framing."""
        return ContainerStreamProducer(self._inner.publish_container_stream(format))

    def encode_audio(
        self,
        name: str,
        input: AudioEncoderInput,
        output: AudioEncoderOutput,
    ) -> AudioProducer:
        """Publish a raw-audio track with an in-process Opus encoder."""
        return AudioProducer(self._inner.encode_audio(name, input, output))

    def encode_video(
        self,
        input: VideoEncoderInput,
        output: VideoEncoderOutput,
    ) -> VideoProducer:
        """Publish a raw-video track with an in-process H.264/H.265 encoder.

        The track is named after the codec (``.avc3`` / ``.hev1``) and its
        catalog rendition is published immediately, read out of the encoder
        itself, so subscribers discover it through the catalog rather than a
        name you pick, and can find it before the first frame exists.
        """
        return VideoProducer(self._inner.encode_video(input, output))

    def publish_track(self, name: str, info: TrackInfo | None = None) -> TrackProducer:
        """Create a track. Send any bytes, no codec validation. ``info`` sets track
        properties (priority, cache, timescale); omit for defaults."""
        return TrackProducer(self._inner.publish_track(name, info))

    def publish_json_snapshot(
        self, name: str, *, delta_ratio: int | None = None, compression: bool = False
    ) -> JsonSnapshotProducer:
        """Publish a JSON snapshot track (lossy latest-value).

        Each update supersedes the last; a late joiner only sees the newest value.
        ``delta_ratio`` controls how aggressively deltas are emitted instead of full
        snapshots (0 disables deltas); ``None`` uses the binding's default. Set
        ``compression`` to DEFLATE-compress each group; the consumer must pass the same
        flag. Advertise the track with :meth:`set_catalog_section` if consumers should
        discover it.
        """
        # Let the record supply delta_ratio's default rather than restating it here.
        config = (
            MoqJsonSnapshotConfig(compression=compression)
            if delta_ratio is None
            else MoqJsonSnapshotConfig(delta_ratio=delta_ratio, compression=compression)
        )
        return JsonSnapshotProducer(self._inner.publish_json_snapshot(name, config))

    def publish_json_stream(self, name: str, *, compression: bool = False) -> JsonStreamProducer:
        """Publish a JSON stream track (lossless append-log).

        Every appended record is preserved and delivered in order. Set ``compression`` to
        DEFLATE-compress the group; the consumer must pass the same flag.
        """
        config = MoqJsonStreamConfig(compression=compression)
        return JsonStreamProducer(self._inner.publish_json_stream(name, config))

    def set_catalog_section(self, name: str, value: Any) -> None:
        """Set or replace an untyped application section in the catalog.

        `value` is any JSON-serializable Python object; it lands as a top-level catalog
        key alongside `video`/`audio` and reaches subscribers via `Catalog.sections`.
        `name` must not be a reserved media section ("video"/"audio"). The catalog is
        republished automatically. Use this to advertise a side-channel track (e.g. a
        transcript or captions track) that the catalog doesn't model natively.
        """
        self._inner.set_catalog_section(name, json.dumps(value))

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
        """Finish the broadcast, closing its tracks and unpublishing it."""
        self._inner.finish()

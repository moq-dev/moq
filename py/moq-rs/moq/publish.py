"""Producer wrappers: publish broadcasts and media tracks."""

from __future__ import annotations

from datetime import timedelta
from typing import TYPE_CHECKING

from moq_ffi import (
    MoqAudioProducer,
    MoqBroadcastDynamic,
    MoqBroadcastProducer,
    MoqGroupProducer,
    MoqGroupRequest,
    MoqTrackDemand,
    MoqTrackDynamic,
    MoqTrackProducer,
    MoqTrackRequest,
    MoqVideoProducer,
)

from ._records import _subscription, _track_info
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
)

if TYPE_CHECKING:
    from .session import Bandwidth, Reservation
    from .subscribe import BroadcastConsumer, GroupConsumer, TrackConsumer


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

    def write_frame(self, payload: bytes, timestamp: timedelta = timedelta(0)) -> None:
        """Write a frame with its presentation timestamp."""
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def finish(self) -> None:
        """Close this group cleanly, marking it complete for subscribers.

        The handle remains so :meth:`abort` can still run.
        """
        self._inner.finish()

    def abort(self, error_code: int) -> None:
        """Abort this group with an application error code."""
        self._inner.abort(error_code)


class TrackDemand:
    """A watch-only handle to whether a published track has subscribers.

    Returned by a producer's ``demand()``. Weak: holding it neither keeps the
    track open nor locks the producer, so a wait can park here while the
    producer keeps publishing. Waits raise ``moq.Error.Closed`` once the track
    is released.
    """

    def __init__(self, inner: MoqTrackDemand) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The name of the track this watches."""
        return self._inner.name()

    def is_used(self) -> bool:
        """Whether the track has at least one active subscriber right now."""
        return self._inner.is_used()

    async def used(self) -> None:
        """Wait until the track has at least one active subscriber."""
        await self._inner.used()

    async def unused(self) -> None:
        """Wait until the track has no active subscribers."""
        await self._inner.unused()


class TrackProducer:
    """Track producer: write arbitrary byte payloads with no codec required.

    Same pattern as moq-boy's status/command tracks.
    """

    def __init__(self, inner: MoqTrackProducer) -> None:
        self._inner = inner

    def demand(self) -> TrackDemand:
        """A watch-only handle to this track's name and whether it has subscribers."""
        return TrackDemand(self._inner.demand())

    def dynamic(self) -> TrackDynamic:
        """Serve fetches for groups that are not currently cached."""
        return TrackDynamic(self._inner.dynamic())

    def append_group(self) -> GroupProducer:
        """Start a new group; write frames into it, then finish()."""
        return GroupProducer(self._inner.append_group())

    def create_group(self, sequence: int) -> GroupProducer:
        """Create a group with an explicit sequence number."""
        return GroupProducer(self._inner.create_group(sequence))

    def write_frame(self, payload: bytes, timestamp: timedelta = timedelta(0)) -> None:
        """Write a single-frame group with its presentation timestamp."""
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def append_datagram(self, payload: bytes, timestamp: timedelta = timedelta(0)) -> int:
        """Send a best-effort datagram and return its sequence number.

        Payloads are capped at 1200 bytes. Datagram delivery requires a datagram-capable
        transport and wire version; there is no stream fallback.
        """
        return self._inner.append_datagram(Frame(payload, timestamp)._ffi())

    def consume(self, subscription: Subscription | None = None) -> TrackConsumer:
        """Create a consumer that reads directly from this producer's track.

        ``subscription`` tunes delivery priority, group range, and staleness; omit for defaults.
        """
        from .subscribe import TrackConsumer

        return TrackConsumer(self._inner.consume(_subscription(subscription)))

    def abort(self, error_code: int) -> None:
        """Abort this track with an application error code."""
        self._inner.abort(error_code)

    def finish(self) -> None:
        """Finish publishing and flush a clean end to subscribers.

        The handle remains so :meth:`abort` can still run.
        """
        self._inner.finish()

    def finish_at(self, final_sequence: int) -> None:
        """Declare the exclusive final group sequence ahead of the live edge."""
        self._inner.finish_at(final_sequence)


class TrackRequest:
    """A subscriber-requested track that hasn't been accepted yet.

    Accept it for raw writes, hand it to :class:`~moq.media.Requested` for :meth:`~moq.media.TrackProducer.audio`
    or :meth:`~moq.media.TrackProducer.video` to publish media (the importer
    accepts it), or abort it to reject the subscriber.
    """

    def __init__(self, inner: MoqTrackRequest) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The requested track name."""
        return self._inner.name()

    def accept(self, info: TrackInfo | None = None) -> TrackProducer:
        """Accept the request as a raw track.

        ``info`` fixes the track's timescale, priority, and cache; omit for defaults.
        """
        return TrackProducer(self._inner.accept(_track_info(info)))

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
    """Async source of uncached group requests for one track.

    Usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqTrackDynamic) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

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


class AudioProducer:
    """Publish raw PCM and let libopus encode it on the way out.

    Built via :meth:`BroadcastProducer.encode_audio`. PCM layout
    (format / sample rate / channels / bitrate / frame duration) is
    fixed at construction; each :meth:`write` call passes only bytes
    and a presentation timestamp.
    """

    def __init__(self, inner: MoqAudioProducer) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The audio track name."""
        return self._inner.name()

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this audio track has subscribers."""
        return TrackDemand(self._inner.demand())

    async def used(self) -> None:
        """Wait until this audio track has at least one active subscriber. Prefer :meth:`demand`."""
        await self._inner.used()

    async def unused(self) -> None:
        """Wait until this audio track has no active subscribers. Prefer :meth:`demand`."""
        await self._inner.unused()

    def reset_epoch(self) -> None:
        """Re-anchor the timeline to the next frame after an idle gap."""
        self._inner.reset_epoch()

    def write(self, frame: AudioFrame) -> None:
        """Push one frame of PCM in the configured input format."""
        self._inner.write(frame)

    def reservation(self) -> Reservation | None:
        """This encoder's bandwidth reservation, if published against a session allocator."""
        inner = self._inner.reservation()
        if inner is None:
            return None
        from .session import Reservation as ReservationType

        return ReservationType(inner)

    def finish(self) -> None:
        """Flush any pending samples and finalize the track."""
        self._inner.finish()


class VideoProducer:
    """Publish raw pictures and let a native encoder compress them on the way out.

    Built via :meth:`BroadcastProducer.encode_video`. Pixel format,
    resolution, and framerate are fixed at construction; each
    :meth:`write` call passes only pixels and a presentation timestamp.
    """

    def __init__(self, inner: MoqVideoProducer) -> None:
        self._inner = inner

    @property
    def name(self) -> str:
        """The video track name."""
        return self._inner.name()

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this video track has subscribers."""
        return TrackDemand(self._inner.demand())

    async def used(self) -> None:
        """Wait until this video track has at least one active subscriber. Prefer :meth:`demand`."""
        await self._inner.used()

    async def unused(self) -> None:
        """Wait until this video track has no active subscribers. Prefer :meth:`demand`."""
        await self._inner.unused()

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
        this. Reach for it only to place the boundaries yourself. Raises if
        the selected encoder cannot force a keyframe; nothing is queued then
        and groups keep their interval.
        """
        self._inner.cut()

    def set_bitrate(self, bitrate: int) -> None:
        """Retune the live encoder, in bits per second.

        Cheap enough to drive from a congestion controller: no keyframe is
        forced. Raises if this backend cannot retune while running. That is
        not fatal: the encoder keeps its current rate.
        """
        self._inner.set_bitrate(bitrate)

    def reservation(self) -> Reservation | None:
        """This encoder's bandwidth reservation, if published against a session allocator."""
        inner = self._inner.reservation()
        if inner is None:
            return None
        from .session import Reservation as ReservationType

        return ReservationType(inner)

    def finish(self) -> None:
        """Flush any frames the codec is holding and finalize the track."""
        self._inner.finish()


class BroadcastDynamic:
    """Async source of tracks requested by subscribers.

    Hold this object while subscriptions to unknown tracks should be accepted.
    Usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqBroadcastDynamic) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

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

    def announce(self, route: Route | None = None) -> None:
        """Advertise this broadcast's exact path as a route.

        Announcing again re-prices the route in place. Until announced, the
        broadcast is invisible and unroutable for local consumers and peers alike.
        """
        self._inner.announce(route if route is not None else Route())

    def unannounce(self) -> None:
        """Retract this broadcast's advertisement, if any, from local consumers and peers alike."""
        self._inner.unannounce()

    def encode_audio(
        self,
        name: str,
        input: AudioEncoderInput,
        output: AudioEncoderOutput,
        *,
        bandwidth: Bandwidth | None = None,
    ) -> AudioProducer:
        """Publish a raw-audio track with an in-process encoder.

        Select the codec with ``moq.AudioCodec.opus()`` or
        ``moq.AudioCodec.aac()``, placed in ``output``.

        Pass ``bandwidth`` to reserve this track's bitrate against the session's
        allocator so a co-resident video encoder sizes itself against what is left.
        """
        return AudioProducer(
            self._inner.encode_audio(name, input, output, None if bandwidth is None else bandwidth._inner)
        )

    def encode_video(
        self,
        input: VideoEncoderInput,
        output: VideoEncoderOutput,
        *,
        bandwidth: Bandwidth | None = None,
    ) -> VideoProducer:
        """Publish a raw-video track with an in-process H.264/H.265 encoder.

        Set ``output.track`` to choose the track name; otherwise one is derived
        from the codec (``.avc3`` / ``.hev1``). The catalog rendition is
        published immediately so subscribers can discover it before the first
        frame exists.

        Pass ``bandwidth`` to reserve this track's configured bitrate and follow
        the grant.
        """
        return VideoProducer(self._inner.encode_video(input, output, None if bandwidth is None else bandwidth._inner))

    def publish_track(self, name: str, info: TrackInfo | None = None) -> TrackProducer:
        """Create a track. Send any bytes, no codec validation. ``info`` sets track
        properties (priority, cache, timescale); omit for defaults."""
        return TrackProducer(self._inner.publish_track(name, _track_info(info)))

    def consume(self) -> BroadcastConsumer:
        """Create a consumer that reads from this broadcast's tracks."""
        from .subscribe import BroadcastConsumer

        return BroadcastConsumer(self._inner.consume())

    def close(self) -> None:
        """End the broadcast for good: retract it and serve no new tracks.

        Tracks already subscribed carry on to their own end. Closing again is a no-op.
        """
        self._inner.close()

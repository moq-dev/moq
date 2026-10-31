"""Catalogs, media importers, and container consumers for a broadcast."""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from datetime import timedelta
from typing import Any, cast

from moq_ffi import MoqAudio as Audio
from moq_ffi import MoqAudioFormat as AudioFormat
from moq_ffi import (
    MoqAudioInit as AudioInit,
)
from moq_ffi import (
    MoqCatalog as Catalog,
)
from moq_ffi import (
    MoqContainer as Container,
)
from moq_ffi import (
    MoqContainerFormat as ContainerFormat,
)
from moq_ffi import (
    MoqContainerInit,
    MoqError,
    MoqMediaCatalogConsumer,
    MoqMediaCatalogProducer,
    MoqMediaContainerProducer,
    MoqMediaFrame,
    MoqMediaGroupConfig,
    MoqMediaGroupConsumer,
    MoqMediaRead,
    MoqMediaTarget,
    MoqMediaTimebase,
    MoqMediaTrackConfig,
    MoqMediaTrackConsumer,
    MoqMediaTrackProducer,
    MoqMediaTrackStreamProducer,
    MoqRendition,
)
from moq_ffi import MoqDecodeStart as DecodeStart
from moq_ffi import MoqDimensions as Dimensions
from moq_ffi import MoqVideo as Video
from moq_ffi import MoqVideoFormat as VideoFormat
from moq_ffi import MoqVideoHint as VideoHint
from moq_ffi import (
    MoqVideoInit as VideoInit,
)
from moq_ffi import (
    MoqVideoProperties as VideoProperties,
)

from . import subscribe as _subscribe
from ._read import End, Gap, _end, _gap
from ._records import Frame, Subscription, _from_us, _subscription
from ._stream import _Stream
from .publish import BroadcastProducer, TrackDemand, TrackRequest
from .subscribe import BroadcastConsumer
from .types import FetchGroupOptions


class Timebase:
    """Share one source's timestamp offset without keeping its broadcast open."""

    def __init__(self, broadcast: BroadcastProducer) -> None:
        self._inner = MoqMediaTimebase(broadcast._inner)


@dataclass(frozen=True)
class ContainerInit:
    """A container format, its leading bytes, and an optional shared source timebase."""

    format: ContainerFormat
    data: bytes
    timebase: Timebase | None = field(default=None, kw_only=True)

    def _ffi(self) -> MoqContainerInit:
        return MoqContainerInit(
            format=self.format, data=self.data, timebase=self.timebase._inner if self.timebase else None
        )


@dataclass(frozen=True)
class Named:
    """Publish a new track with a chosen name, or a format-derived unique name."""

    name: str | None = None

    def _ffi(self) -> MoqMediaTarget:
        return cast(MoqMediaTarget, MoqMediaTarget.NAMED(name=self.name))


@dataclass(frozen=True)
class Requested:
    """Publish the track named by a subscriber's pending request."""

    request: TrackRequest

    def _ffi(self) -> MoqMediaTarget:
        return cast(MoqMediaTarget, MoqMediaTarget.REQUESTED(request=self.request._inner))


Target = Named | Requested
"""The named or requested track an importer publishes."""


@dataclass(frozen=True)
class MediaFrame:
    """A container-decoded payload, presentation timestamp, and keyframe flag."""

    payload: bytes
    timestamp: timedelta
    keyframe: bool

    @staticmethod
    def _from_ffi(frame: MoqMediaFrame) -> MediaFrame:
        return MediaFrame(frame.payload, _from_us(frame.timestamp_us), frame.keyframe)


Read = MediaFrame | Gap | End
"""One encoded frame, a lost span, or an encoder end, in delivery order."""


class CatalogProducer:
    """Update a broadcast's catalog without keeping the broadcast open."""

    def __init__(self, broadcast: BroadcastProducer) -> None:
        self._inner = MoqMediaCatalogProducer(broadcast._inner)

    def now(self) -> timedelta:
        """Read the broadcast clock, fixing it for subsequent media producers."""
        return _from_us(self._inner.now())

    def set_video_properties(self, properties: VideoProperties) -> None:
        """Replace the video properties shared by every rendition."""
        self._inner.set_video_properties(properties)

    def set_section(self, name: str, value: Any) -> None:
        """Set an application catalog section from a JSON-serializable value."""
        self._inner.set_section(name, json.dumps(value))

    def remove_section(self, name: str) -> None:
        """Remove an application catalog section if present."""
        self._inner.remove_section(name)

    def finish(self) -> None:
        """Finish the catalog, which ends the broadcast's content.

        Finish every track first. The broadcast stays announced and keeps serving its
        finished tracks to late subscribers until its ``close`` takes it offline; how long
        to keep it up is yours to decide.
        """
        self._inner.finish()


class TrackProducer:
    """Publish encoded media frames on a single track, one payload at a time.

    Construct with :meth:`audio` or :meth:`video`. Push each encoded frame with
    :meth:`write_frame`, then :meth:`finish` when the stream ends.
    """

    @classmethod
    def audio(cls, broadcast: BroadcastProducer, init: AudioInit, *, target: Target = Named()) -> TrackProducer:
        """Import complete encoded audio frames on a named or requested track."""
        return cls(MoqMediaTrackProducer.audio(broadcast._inner, target._ffi(), init))

    @classmethod
    def video(cls, broadcast: BroadcastProducer, init: VideoInit, *, target: Target = Named()) -> TrackProducer:
        """Import complete encoded video frames on a named or requested track."""
        return cls(MoqMediaTrackProducer.video(broadcast._inner, target._ffi(), init))

    def __init__(self, inner: MoqMediaTrackProducer) -> None:
        self._inner = inner

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this media track has subscribers."""
        return TrackDemand(self._inner.demand())

    def write_frame(self, payload: bytes, timestamp: timedelta) -> None:
        """Write one encoded frame with a presentation timestamp."""
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def discontinuity(self) -> None:
        """Mark a timeline break, preserving the advertised jitter and delay."""
        self._inner.discontinuity()

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

    Unlike :class:`TrackProducer` there is no per-frame timestamp: a container
    carries its tracks' timing itself.
    """

    def __init__(self, broadcast: BroadcastProducer, init: ContainerInit) -> None:
        self._inner = MoqMediaContainerProducer(broadcast._inner, init._ffi())

    def write(self, payload: bytes) -> None:
        """Write container bytes; the container recovers its own framing, so chunks may split anywhere."""
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


class TrackStreamProducer:
    """Wraps MoqMediaTrackStreamProducer: feed a raw byte stream (e.g. Annex-B
    H.264) and let the importer infer frame boundaries.

    Construct with :meth:`video`. Unlike
    :class:`TrackProducer`, no per-frame timestamps are needed; just push
    encoder bytes as they arrive.
    """

    @classmethod
    def video(cls, broadcast: BroadcastProducer, init: VideoInit, *, target: Target = Named()) -> TrackStreamProducer:
        """Import a video byte stream, inferring frame boundaries."""
        return cls(MoqMediaTrackStreamProducer.video(broadcast._inner, target._ffi(), init))

    def __init__(self, inner: MoqMediaTrackStreamProducer) -> None:
        self._inner = inner

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this media track has subscribers."""
        return TrackDemand(self._inner.demand())

    def write(self, payload: bytes) -> None:
        """Push raw stream bytes; whole frames are emitted as they complete."""
        self._inner.write(payload)

    def finish(self) -> None:
        """Finish publishing and flush a clean end to subscribers."""
        self._inner.finish()


def _rendition(id: str, rendition: Video | Audio) -> MoqRendition:
    if isinstance(rendition, Video):
        return cast(MoqRendition, MoqRendition.VIDEO(id=id, config=rendition))
    if isinstance(rendition, Audio):
        return cast(MoqRendition, MoqRendition.AUDIO(id=id, config=rendition))
    raise TypeError(f"expected a catalog Video or Audio rendition, got {type(rendition).__name__}")


class TrackConsumer(_Stream[Read]):
    """Async-iterable encoded frames and the :class:`Gap` and :class:`End` breaks between them.

    Construct with :meth:`subscribe`. Iterate with ``async for``;
    usable as an async context manager that cancels on exit.
    """

    @classmethod
    async def subscribe(
        cls,
        broadcast: BroadcastConsumer,
        id: str,
        rendition: Video | Audio,
        *,
        subscription: Subscription | None = None,
    ) -> TrackConsumer:
        """Subscribe to the catalog rendition ``id`` and decode its container, following its broadcast and track."""
        config = MoqMediaTrackConfig(rendition=_rendition(id, rendition), subscription=_subscription(subscription))
        return cls(await MoqMediaTrackConsumer.subscribe(broadcast._inner, config))

    def __init__(self, inner: MoqMediaTrackConsumer) -> None:
        self._inner = inner

    async def _next(self) -> Read | None:
        value = await self._inner.next()
        if value is None:
            return None
        if isinstance(value, MoqMediaRead.FRAME):
            return MediaFrame._from_ffi(value.frame)
        if isinstance(value, MoqMediaRead.GAP):
            return _gap(value.timestamp_us, value.duration)
        if isinstance(value, MoqMediaRead.END):
            return _end(value.end_us)
        raise TypeError(f"unknown media read: {value!r}")

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering frames."""
        self._inner.cancel()


class GroupConsumer(_Stream[MediaFrame]):
    """Async iterator of decoded :class:`MediaFrame` within a single group.

    Construct with :meth:`fetch`, or decode a raw :class:`~moq.GroupConsumer`,
    such as one from :meth:`~moq.TrackConsumer.recv_group` for arrival-order
    reads. Finite: iteration ends after the group's last frame. Usable as an
    async context manager that cancels on exit.
    """

    def __init__(self, group: _subscribe.GroupConsumer, container: Container) -> None:
        """Decode ``group`` with the track's container, as each frame lands.

        Frame timestamps are container media time, not the transport's. Which
        groups arrive, and how long a stalled one stays readable, is the
        subscription's ``max_delay``. Takes over ``group``, whose handle is
        closed afterward; raises ``Error.AlreadyCommitted`` if it has already
        read a frame.
        """
        self._inner = MoqMediaGroupConsumer(group._inner, container)

    @classmethod
    def _from_inner(cls, inner: MoqMediaGroupConsumer) -> GroupConsumer:
        self = cls.__new__(cls)
        self._inner = inner
        return self

    @classmethod
    async def fetch(
        cls,
        broadcast: BroadcastConsumer,
        id: str,
        rendition: Video | Audio,
        sequence: int,
        *,
        options: FetchGroupOptions | None = None,
    ) -> GroupConsumer:
        """Fetch and decode one group of the catalog rendition ``id``, following its broadcast and track."""
        config = MoqMediaGroupConfig(rendition=_rendition(id, rendition), sequence=sequence, options=options)
        return cls._from_inner(await MoqMediaGroupConsumer.fetch(broadcast._inner, config))

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    async def _next(self) -> MediaFrame | None:
        frame = await self._inner.next()
        return None if frame is None else MediaFrame._from_ffi(frame)

    def cancel(self) -> None:
        """Cancel reading this group and stop delivering frames."""
        self._inner.cancel()


class CatalogConsumer(_Stream[Catalog]):
    """Async-iterable stream of :class:`Catalog` snapshots as the broadcast updates.

    Construct with :meth:`subscribe`. Each item is the latest
    catalog describing the broadcast's tracks; usable as an async context manager.
    """

    @classmethod
    async def subscribe(cls, broadcast: BroadcastConsumer) -> CatalogConsumer:
        """Subscribe to the broadcast's catalog snapshots."""
        return cls(await MoqMediaCatalogConsumer.subscribe(broadcast._inner))

    def __init__(self, inner: MoqMediaCatalogConsumer) -> None:
        self._inner = inner

    async def _next(self) -> Catalog | None:
        return await self._inner.next()

    def cancel(self) -> None:
        """Cancel the catalog subscription and stop delivering updates."""
        self._inner.cancel()


async def catalog(broadcast: BroadcastConsumer) -> Catalog:
    """Read the first catalog snapshot, then release the subscription."""
    consumer = await CatalogConsumer.subscribe(broadcast)
    try:
        return await anext(consumer)
    except StopAsyncIteration:
        raise MoqError.Closed() from None
    finally:
        consumer.cancel()


__all__ = [
    "Named",
    "Requested",
    "Target",
    "MediaFrame",
    "Read",
    "Gap",
    "End",
    "CatalogProducer",
    "CatalogConsumer",
    "TrackProducer",
    "TrackStreamProducer",
    "ContainerProducer",
    "TrackConsumer",
    "GroupConsumer",
    "Audio",
    "Video",
    "Catalog",
    "Container",
    "Dimensions",
    "DecodeStart",
    "AudioFormat",
    "VideoFormat",
    "ContainerFormat",
    "VideoHint",
    "VideoProperties",
    "AudioInit",
    "VideoInit",
    "ContainerInit",
    "Timebase",
    "catalog",
]

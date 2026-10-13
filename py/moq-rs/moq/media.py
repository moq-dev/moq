"""Catalogs, media importers, and container consumers for a broadcast."""

from __future__ import annotations

import json
from dataclasses import dataclass
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
    MoqContainerInit as ContainerInit,
)
from moq_ffi import MoqDimensions as Dimensions
from moq_ffi import (
    MoqError,
    MoqMediaCatalogConsumer,
    MoqMediaCatalogProducer,
    MoqMediaContainerConfig,
    MoqMediaContainerConsumer,
    MoqMediaContainerGroupConfig,
    MoqMediaContainerGroupConsumer,
    MoqMediaContainerProducer,
    MoqMediaContainerStreamProducer,
    MoqMediaFrame,
    MoqMediaTarget,
    MoqMediaTrackProducer,
    MoqMediaTrackStreamProducer,
)
from moq_ffi import MoqVideo as Video
from moq_ffi import MoqVideoFormat as VideoFormat
from moq_ffi import MoqVideoHint as VideoHint
from moq_ffi import (
    MoqVideoInit as VideoInit,
)
from moq_ffi import (
    MoqVideoProperties as VideoProperties,
)

from ._records import Frame, Subscription, _from_us, _subscription, _to_us
from .publish import BroadcastProducer, TrackDemand, TrackRequest
from .subscribe import BroadcastConsumer
from .types import FetchGroupOptions


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


class CatalogProducer:
    """Update a broadcast's catalog without keeping the broadcast open."""

    def __init__(self, broadcast: BroadcastProducer) -> None:
        self._inner = MoqMediaCatalogProducer(broadcast._inner)

    def set_video_properties(self, properties: VideoProperties) -> None:
        """Replace the video properties shared by every rendition."""
        self._inner.set_video_properties(properties)

    def set_section(self, name: str, value: Any) -> None:
        """Set an application catalog section from a JSON-serializable value."""
        self._inner.set_section(name, json.dumps(value))

    def remove_section(self, name: str) -> None:
        """Remove an application catalog section if present."""
        self._inner.remove_section(name)


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

    def write_frame(self, payload: bytes, timestamp: timedelta = timedelta(0)) -> None:
        """Write one encoded frame with a presentation timestamp."""
        self._inner.write_frame(Frame(payload, timestamp)._ffi())

    def flush(self, timestamp: timedelta) -> None:
        """Record a local encoder's frame handoff on the broadcast media clock.

        Call this after ``write_frame`` only for encoded live output. File, pipe,
        and network imports should leave their jitter estimate clock free.
        """
        self._inner.flush(_to_us(timestamp, "timestamp"))

    def discontinuity(self) -> None:
        """Mark a timeline break and restart handoff measurement, preserving advertised jitter."""
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
        self._inner = MoqMediaContainerProducer(broadcast._inner, init)

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
    """Publish a container fed by a raw byte stream, which recovers its own framing."""

    def __init__(self, broadcast: BroadcastProducer, format: ContainerFormat) -> None:
        self._inner = MoqMediaContainerStreamProducer(broadcast._inner, format)

    def write(self, payload: bytes) -> None:
        """Push raw container bytes; chunk boundaries don't matter."""
        self._inner.write(payload)

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


class ContainerConsumer:
    """Async-iterable stream of decoded :class:`MediaFrame` in decode order.

    Construct with :meth:`subscribe`. Iterate with ``async for``;
    usable as an async context manager that cancels on exit.
    """

    @classmethod
    async def subscribe(
        cls, broadcast: BroadcastConsumer, name: str, container: Container, *, subscription: Subscription | None = None
    ) -> ContainerConsumer:
        """Subscribe to a media track and decode its container."""
        config = MoqMediaContainerConfig(name=name, container=container, subscription=_subscription(subscription))
        return cls(await MoqMediaContainerConsumer.subscribe(broadcast._inner, config))

    def __init__(self, inner: MoqMediaContainerConsumer) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> MediaFrame:
        frame = await self._inner.next()
        if frame is None:
            raise StopAsyncIteration
        return MediaFrame._from_ffi(frame)

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering frames."""
        self._inner.cancel()


class ContainerGroupConsumer:
    """Async iterator of decoded :class:`MediaFrame` within a single fetched group.

    Construct with :meth:`fetch`. Finite: iteration ends
    after the group's last frame. Usable as an async context manager that cancels
    on exit.
    """

    @classmethod
    async def fetch(
        cls,
        broadcast: BroadcastConsumer,
        name: str,
        sequence: int,
        container: Container,
        *,
        options: FetchGroupOptions | None = None,
    ) -> ContainerGroupConsumer:
        """Fetch and decode exactly one media group."""
        config = MoqMediaContainerGroupConfig(name=name, sequence=sequence, container=container, options=options)
        return cls(await MoqMediaContainerGroupConsumer.fetch(broadcast._inner, config))

    def __init__(self, inner: MoqMediaContainerGroupConsumer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> MediaFrame:
        frame = await self._inner.next()
        if frame is None:
            raise StopAsyncIteration
        return MediaFrame._from_ffi(frame)

    def cancel(self) -> None:
        """Cancel reading this group and stop delivering frames."""
        self._inner.cancel()


class CatalogConsumer:
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

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Catalog:
        catalog = await self._inner.next()
        if catalog is None:
            raise StopAsyncIteration
        return catalog

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
    "CatalogProducer",
    "CatalogConsumer",
    "TrackProducer",
    "TrackStreamProducer",
    "ContainerProducer",
    "ContainerStreamProducer",
    "ContainerConsumer",
    "ContainerGroupConsumer",
    "Audio",
    "Video",
    "Catalog",
    "Container",
    "Dimensions",
    "AudioFormat",
    "VideoFormat",
    "ContainerFormat",
    "VideoHint",
    "VideoProperties",
    "AudioInit",
    "VideoInit",
    "ContainerInit",
    "catalog",
]

"""Consumer wrappers for broadcasts, catalogs, and media tracks."""

from __future__ import annotations

import json
from typing import Any

from moq_ffi import (
    MoqAudioConsumer,
    MoqBroadcastConsumer,
    MoqCatalogConsumer,
    MoqGroupConsumer,
    MoqJsonConfig,
    MoqJsonConsumer,
    MoqJsonStreamConfig,
    MoqJsonStreamConsumer,
    MoqMediaConsumer,
    MoqTrackConsumer,
)

from .types import (
    Audio,
    AudioDecoderOutput,
    AudioFrame,
    Catalog,
    Container,
    Datagram,
    FetchGroupOptions,
    Frame,
    Subscription,
    TrackInfo,
    Video,
)


class MediaConsumer:
    """Wraps MoqMediaConsumer as an async iterator of Frame."""

    def __init__(self, inner: MoqMediaConsumer) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> Frame:
        frame = await self._inner.next()
        if frame is None:
            raise StopAsyncIteration
        return frame

    def cancel(self) -> None:
        self._inner.cancel()


class GroupConsumer:
    """Async iterator of timestamped frames within a single group."""

    def __init__(self, inner: MoqGroupConsumer) -> None:
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

    async def __anext__(self) -> Frame:
        frame = await self._inner.read_frame()
        if frame is None:
            raise StopAsyncIteration
        return frame

    async def read_frame(self) -> Frame | None:
        """Read the next timestamped frame. Returns `None` when the group ends."""
        return await self._inner.read_frame()

    def cancel(self) -> None:
        self._inner.cancel()


class TrackConsumer:
    """Async iterator of groups from a track.

    Each group is itself an async iterator of timestamped frames. Same pattern as
    moq-boy's status/command tracks (one frame per group), but multi-frame
    groups are also supported.
    """

    def __init__(self, inner: MoqTrackConsumer) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> GroupConsumer:
        group = await self.recv_group()
        if group is None:
            raise StopAsyncIteration
        return group

    async def recv_group(self) -> GroupConsumer | None:
        """Return the next group in arrival order. Returns `None` when the track ends.

        Groups are returned as they arrive on the wire, which may be out of sequence
        order. Use this for live consumption where latency matters more than order.
        """
        group = await self._inner.recv_group()
        if group is None:
            return None
        return GroupConsumer(group)

    async def next_group(self) -> GroupConsumer | None:
        """Return the next group in sequence order, skipping forward if behind.

        Returns `None` when the track ends. Use this when order matters more than
        latency; `recv_group` is preferred for live consumption.
        """
        group = await self._inner.next_group()
        if group is None:
            return None
        return GroupConsumer(group)

    async def read_frame(self) -> Frame | None:
        """Read the first timestamped frame of the next group.

        Convenience for tracks using one-frame-per-group (like moq-boy's
        status/command tracks). Returns `None` when the track ends.
        """
        return await self._inner.read_frame()

    async def recv_datagram(self) -> Datagram | None:
        """Receive the next best-effort datagram in arrival order.

        Returns ``None`` when the track ends. Datagrams are unavailable over stream-only
        transports and older wire versions.
        """
        return await self._inner.recv_datagram()

    async def info(self) -> TrackInfo:
        """Return the publisher-side track properties."""
        return await self._inner.info()

    def update(self, subscription: Subscription) -> None:
        """Change this subscriber's delivery preferences."""
        self._inner.update(subscription)

    def cancel(self) -> None:
        self._inner.cancel()


class AudioConsumer:
    """Async iterator of decoded audio frames.

    Built via :meth:`BroadcastConsumer.subscribe_audio`. The PCM layout
    is fixed by the :class:`AudioDecoderOutput` passed at subscribe
    time; each frame's ``data`` is raw bytes in that format.
    """

    def __init__(self, inner: MoqAudioConsumer) -> None:
        self._inner = inner

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> AudioFrame:
        frame = await self._inner.next()
        if frame is None:
            raise StopAsyncIteration
        return frame

    def cancel(self) -> None:
        self._inner.cancel()


class JsonConsumer:
    """Async iterator over a JSON snapshot track, yielding the latest value (lossy).

    Built via :meth:`BroadcastConsumer.subscribe_json`. Each item is a parsed Python object.
    A consumer that has fallen behind collapses the backlog and yields only the latest value.
    """

    def __init__(self, inner: MoqJsonConsumer) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> Any:
        value = await self._inner.next()
        if value is None:
            raise StopAsyncIteration
        return json.loads(value)

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()


class JsonStreamConsumer:
    """Async iterator over a JSON stream track, yielding every record in order (lossless).

    Built via :meth:`BroadcastConsumer.subscribe_json_stream`. Each item is a parsed Python object.
    """

    def __init__(self, inner: MoqJsonStreamConsumer) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> Any:
        value = await self._inner.next()
        if value is None:
            raise StopAsyncIteration
        return json.loads(value)

    def cancel(self) -> None:
        """Cancel all current and future next() calls."""
        self._inner.cancel()


class CatalogConsumer:
    """Wraps MoqCatalogConsumer as an async iterator of Catalog."""

    def __init__(self, inner: MoqCatalogConsumer) -> None:
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
        self._inner.cancel()


class BroadcastConsumer:
    """Wraps MoqBroadcastConsumer with convenience methods."""

    def __init__(self, inner: MoqBroadcastConsumer) -> None:
        self._inner = inner

    async def subscribe_catalog(self) -> CatalogConsumer:
        return CatalogConsumer(await self._inner.subscribe_catalog())

    async def subscribe_track(self, name: str, subscription: Subscription | None = None) -> TrackConsumer:
        """Subscribe to a track and receive arbitrary byte payloads.

        ``subscription`` tunes delivery priority, group ordering priority, and group range; omit for defaults.
        """
        return TrackConsumer(await self._inner.subscribe_track(name, subscription))

    async def subscribe_json(self, name: str, *, compression: bool = False) -> JsonConsumer:
        """Subscribe to a JSON snapshot track (lossy latest-value).

        Yields parsed Python objects. Pass the same ``compression`` the producer used.
        """
        config = MoqJsonConfig(delta_ratio=0, compression=compression)
        return JsonConsumer(await self._inner.subscribe_json(name, config))

    async def subscribe_json_stream(self, name: str, *, compression: bool = False) -> JsonStreamConsumer:
        """Subscribe to a JSON stream track (lossless append-log).

        Yields parsed Python objects in order. Pass the same ``compression`` the producer used.
        """
        config = MoqJsonStreamConfig(compression=compression)
        return JsonStreamConsumer(await self._inner.subscribe_json_stream(name, config))

    async def fetch_group(
        self,
        name: str,
        sequence: int,
        options: FetchGroupOptions | None = None,
    ) -> GroupConsumer:
        """Fetch one complete group by track name and group sequence.

        This does not hold a live subscription. The returned group may still be
        receiving frames, so iterate it until completion.
        """
        return GroupConsumer(await self._inner.fetch_group(name, sequence, options))

    async def subscribe_media(
        self,
        name: str,
        track: Video | Audio | Container,
        max_latency_ms: int = 10000,
        subscription: Subscription | None = None,
    ) -> MediaConsumer:
        """Subscribe to a media track, delivering frames in decode order.

        ``track`` is either the catalog entry for this track (e.g.
        ``catalog.video[name]``), whose ``container`` describes how to parse the
        bitstream, or a :class:`Container` directly. Pass a bare container for the
        dynamic flow, where you subscribe before the catalog exists.
        ``max_latency_ms`` bounds buffering before a stalled GoP is skipped.
        ``subscription`` tunes delivery priority, group ordering priority, and group range; omit for defaults.
        """
        container = track if isinstance(track, Container) else track.container
        return MediaConsumer(await self._inner.subscribe_media(name, container, max_latency_ms, subscription))

    async def subscribe_audio(
        self,
        name: str,
        catalog_audio: Audio,
        output: AudioDecoderOutput,
    ) -> AudioConsumer:
        """Subscribe to a raw-audio track; samples come back in the format
        declared by ``output``.

        ``catalog_audio`` comes from the catalog (e.g.
        ``await broadcast.catalog()`` followed by
        ``catalog.audio[name]``). Use ``output.latency_max_ms`` to
        control how aggressively stalled groups get skipped. That's
        the congestion-control knob. (Named ``_max`` to leave room for
        a future ``latency_min_ms`` jitter-buffer floor.)
        """
        return AudioConsumer(await self._inner.subscribe_audio(name, catalog_audio, output))

    async def catalog(self) -> Catalog:
        """Convenience: subscribe and return the first catalog."""
        consumer = await self.subscribe_catalog()
        return await anext(consumer)

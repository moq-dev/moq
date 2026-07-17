"""Consumer wrappers — subscribe to broadcasts, catalogs, and media tracks."""

from __future__ import annotations

import json
from typing import Any

from moq_ffi import (
    Container,
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

from .types import Audio, AudioDecoderOutput, AudioFrame, Catalog, Frame


class MediaConsumer:
    """Wraps MoqMediaConsumer as an async iterator of Frame."""

    def __init__(self, inner: MoqMediaConsumer) -> None:
        self._inner = inner

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
    """Async iterator of byte payloads within a single group."""

    def __init__(self, inner: MoqGroupConsumer) -> None:
        self._inner = inner

    @property
    def sequence(self) -> int:
        """The sequence number of this group within the track."""
        return self._inner.sequence()

    def __aiter__(self):
        return self

    async def __anext__(self) -> bytes:
        frame = await self._inner.read_frame()
        if frame is None:
            raise StopAsyncIteration
        return frame

    def cancel(self) -> None:
        self._inner.cancel()


class TrackConsumer:
    """Async iterator of groups from a track.

    Each group is itself an async iterator of byte payloads. Same pattern as
    moq-boy's status/command tracks (one frame per group), but multi-frame
    groups are also supported.
    """

    def __init__(self, inner: MoqTrackConsumer) -> None:
        self._inner = inner

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

    async def read_frame(self) -> bytes | None:
        """Read the first frame of the next group.

        Convenience for tracks using one-frame-per-group (like moq-boy's
        status/command tracks). Returns `None` when the track ends.
        """
        return await self._inner.read_frame()

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

    def subscribe_catalog(self) -> CatalogConsumer:
        return CatalogConsumer(self._inner.subscribe_catalog())

    def subscribe_track(self, name: str) -> TrackConsumer:
        """Subscribe to a track — receive arbitrary byte payloads."""
        return TrackConsumer(self._inner.subscribe_track(name))

    def subscribe_json(self, name: str, *, compression: bool = False) -> JsonConsumer:
        """Subscribe to a JSON snapshot track (lossy latest-value).

        Yields parsed Python objects. Pass the same ``compression`` the producer used.
        """
        config = MoqJsonConfig(delta_ratio=0, compression=compression)
        return JsonConsumer(self._inner.subscribe_json(name, config))

    def subscribe_json_stream(self, name: str, *, compression: bool = False) -> JsonStreamConsumer:
        """Subscribe to a JSON stream track (lossless append-log).

        Yields parsed Python objects in order. Pass the same ``compression`` the producer used.
        """
        config = MoqJsonStreamConfig(compression=compression)
        return JsonStreamConsumer(self._inner.subscribe_json_stream(name, config))

    def subscribe_media(self, name: str, container: Container, max_latency_ms: int) -> MediaConsumer:
        return MediaConsumer(self._inner.subscribe_media(name, container, max_latency_ms))

    def subscribe_audio(
        self,
        name: str,
        catalog_audio: Audio,
        output: AudioDecoderOutput,
    ) -> AudioConsumer:
        """Subscribe to a raw-audio track; samples come back in the format
        declared by ``output``.

        ``catalog_audio`` comes from the catalog (e.g.
        ``await broadcast.catalog()`` followed by
        ``catalog.audio[name]``). Only Opus tracks are currently supported.
        Use ``output.latency_max_ms`` to
        control how aggressively stalled groups get skipped. That's
        the congestion-control knob. (Named ``_max`` to leave room for
        a future ``latency_min_ms`` jitter-buffer floor.)
        """
        return AudioConsumer(self._inner.subscribe_audio(name, catalog_audio, output))

    async def catalog(self) -> Catalog:
        """Convenience: subscribe and return the first catalog."""
        consumer = self.subscribe_catalog()
        return await anext(consumer)

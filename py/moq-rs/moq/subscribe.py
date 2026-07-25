"""Consumer wrappers for broadcasts, catalogs, and media tracks."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import Any

from moq_ffi import (
    MoqAudioConsumer,
    MoqBroadcastConsumer,
    MoqCatalogConsumer,
    MoqGroupConsumer,
    MoqJsonSnapshotConfig,
    MoqJsonSnapshotConsumer,
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
    MediaFrame,
    Route,
    Subscription,
    TrackInfo,
    Video,
)


class MediaConsumer:
    """Async-iterable stream of decoded :class:`MediaFrame` in decode order.

    Built via :meth:`BroadcastConsumer.subscribe_media`. Iterate with ``async for``;
    usable as an async context manager that cancels on exit.
    """

    def __init__(self, inner: MoqMediaConsumer) -> None:
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
        return frame

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering frames."""
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
        """Cancel reading this group and stop delivering frames."""
        self._inner.cancel()


class TrackConsumer:
    """Async iterator of groups from a track, in sequence order.

    Iterating yields groups via :meth:`next_group`. Use :meth:`recv_group` (or
    :meth:`groups_as_arrived`) for arrival order instead.

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
        group = await self.next_group()
        if group is None:
            raise StopAsyncIteration
        return group

    async def groups_as_arrived(self) -> AsyncIterator[GroupConsumer]:
        """Iterate groups in arrival order, including out-of-sequence deliveries.

        The default iteration uses sequence order instead. Use this for live
        consumption where latency matters more than order.
        """
        while True:
            group = await self.recv_group()
            if group is None:
                return
            yield group

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

        Returns `None` when the track ends. This is what the default iteration
        yields; use `recv_group` when latency matters more than order.
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

    def info(self) -> TrackInfo:
        """Return the publisher-side track properties."""
        return self._inner.info()

    def update(self, subscription: Subscription) -> None:
        """Change this subscriber's delivery preferences."""
        self._inner.update(subscription)

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering groups."""
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
        """Cancel the subscription and stop delivering audio frames."""
        self._inner.cancel()


class JsonSnapshotConsumer:
    """Async iterator over a JSON snapshot track, yielding the latest value (lossy).

    Built via :meth:`BroadcastConsumer.subscribe_json_snapshot`. Each item is a parsed Python object.
    A consumer that has fallen behind collapses the backlog and yields only the latest value.
    """

    def __init__(self, inner: MoqJsonSnapshotConsumer) -> None:
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
    """Async-iterable stream of :class:`Catalog` snapshots as the broadcast updates.

    Built via :meth:`BroadcastConsumer.subscribe_catalog`. Each item is the latest
    catalog describing the broadcast's tracks; usable as an async context manager.
    """

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
        """Cancel the catalog subscription and stop delivering updates."""
        self._inner.cancel()


class BroadcastConsumer:
    """The consume side of one broadcast: subscribe to its tracks, catalog, and media.

    Built by resolving an announcement or request. Read its :attr:`route`, then use
    the ``subscribe_*`` / :meth:`fetch_group` methods to pull tracks and media.
    """

    def __init__(self, inner: MoqBroadcastConsumer) -> None:
        self._inner = inner
        self._route_watch = None

    @property
    def route(self) -> Route:
        """The route the broadcast currently takes to reach this origin.

        ``route.hops`` is the chain of relay origin ids (oldest first) and
        ``route.cost`` the publisher's advertised preference (lower wins).
        """
        return self._inner.route()

    async def route_changed(self) -> Route | None:
        """Wait for the broadcast's route to change.

        The first call returns the current route immediately; each later call
        blocks until it changes again (e.g. an upstream failover). Returns
        ``None`` once the broadcast ends.
        """
        if self._route_watch is None:
            self._route_watch = self._inner.route_updates()
        return await self._route_watch.next()

    async def subscribe_catalog(self) -> CatalogConsumer:
        """Subscribe to the broadcast's catalog, async-iterating snapshots as it changes."""
        return CatalogConsumer(await self._inner.subscribe_catalog())

    async def subscribe_track(self, name: str, subscription: Subscription | None = None) -> TrackConsumer:
        """Subscribe to a track and receive arbitrary byte payloads.

        ``subscription`` tunes delivery priority, group ordering priority, and group range; omit for defaults.
        """
        return TrackConsumer(await self._inner.subscribe_track(name, subscription))

    async def subscribe_json_snapshot(self, name: str, *, compression: bool = False) -> JsonSnapshotConsumer:
        """Subscribe to a JSON snapshot track (lossy latest-value).

        Yields parsed Python objects. Pass the same ``compression`` the producer used.
        """
        # delta_ratio is producer-only, so leave it at its default here.
        config = MoqJsonSnapshotConfig(compression=compression)
        return JsonSnapshotConsumer(await self._inner.subscribe_json_snapshot(name, config))

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
        subscription: Subscription | None = None,
    ) -> MediaConsumer:
        """Subscribe to a media track, delivering frames in decode order.

        ``track`` is either the catalog entry for this track (e.g.
        ``catalog.video[name]``), whose ``container`` describes how to parse the
        bitstream, or a :class:`Container` directly. Pass a bare container for the
        dynamic flow, where you subscribe before the catalog exists.
        ``subscription`` tunes delivery priority, group ordering priority, group
        range, and the latency budget; omit for defaults. Raise
        :attr:`Subscription.latency_max_ms` to buffer instead of skipping a
        stalled group.
        """
        container = track if isinstance(track, Container) else track.container
        return MediaConsumer(await self._inner.subscribe_media(name, container, subscription))

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
        ``catalog.audio[name]``). Only Opus tracks are currently supported.
        Use ``output.latency_max_ms`` to
        control how aggressively stalled groups get skipped. That's
        the congestion-control knob. (Named ``_max`` to leave room for
        a future ``latency_min_ms`` jitter-buffer floor.)
        """
        return AudioConsumer(await self._inner.subscribe_audio(name, catalog_audio, output))

    async def catalog(self) -> Catalog:
        """Convenience: subscribe and return the first catalog."""
        consumer = await self.subscribe_catalog()
        return await anext(consumer)

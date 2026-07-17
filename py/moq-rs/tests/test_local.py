"""Local pub/sub tests: no network required."""

import asyncio
import struct
from typing import cast

import moq
import pytest


def opus_head() -> bytes:
    """Build a valid OpusHead init buffer (RFC 7845)."""
    return (
        b"OpusHead"
        + bytes([1, 2])  # version, channels
        + struct.pack("<H", 0)  # pre-skip
        + struct.pack("<I", 48000)  # sample rate
        + struct.pack("<H", 0)  # output gain
        + bytes([0])  # channel mapping
    )


def h264_init() -> bytes:
    """H.264 Annex B init with SPS + PPS (1280x720, High profile)."""
    sps = bytes(
        [
            0x00,
            0x00,
            0x00,
            0x01,  # start code
            0x67,
            0x64,
            0x00,
            0x1F,
            0xAC,
            0x24,
            0x84,
            0x01,
            0x40,
            0x16,
            0xEC,
            0x04,
            0x40,
            0x00,
            0x00,
            0x03,
            0x00,
            0x40,
            0x00,
            0x00,
            0x0C,
            0x23,
            0xC6,
            0x0C,
            0x92,
        ]
    )
    pps = bytes(
        [
            0x00,
            0x00,
            0x00,
            0x01,  # start code
            0x68,
            0xEE,
            0x32,
            0xC8,
            0xB0,
        ]
    )
    return sps + pps


def test_origin_lifecycle():
    origin = moq.OriginProducer()
    _consumer = origin.consume()


def test_publish_media_lifecycle():
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_media("opus", opus_head())
    media.write_frame(b"opus frame", 1000)
    media.finish()
    broadcast.finish()


def test_unknown_format():
    broadcast = moq.BroadcastProducer()
    with pytest.raises(Exception):
        broadcast.publish_media("nope", b"")


async def test_local_publish_consume_audio():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_media("opus", opus_head())
    _announce = origin.announce("live", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.path == "live"

        catalog = await announcement.broadcast.catalog()

        assert len(catalog.audio) == 1
        assert len(catalog.video) == 0

        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        assert audio.codec == "opus"
        assert audio.sample_rate == 48000
        assert audio.channel_count == 2

        media_consumer = await announcement.broadcast.subscribe_media(track_name, audio)

        payload = b"opus audio payload data"
        media.write_frame(payload, 1_000_000)

        async for frame in media_consumer:
            assert frame.payload == payload
            assert frame.timestamp_us == 1_000_000
            break

        break


async def test_video_publish_consume():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_media("avc3", h264_init())
    _announce = origin.announce("video-test", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        catalog = await announcement.broadcast.catalog()

        assert len(catalog.video) == 1
        assert len(catalog.audio) == 0

        track_name = list(catalog.video.keys())[0]
        video = catalog.video[track_name]
        assert video.codec.startswith("avc1.") or video.codec.startswith("avc3.")
        assert video.coded is not None
        assert video.coded.width == 1280
        assert video.coded.height == 720

        media_consumer = await announcement.broadcast.subscribe_media(track_name, video)

        keyframe = bytes([0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC])
        media.write_frame(keyframe, 0)

        async for frame in media_consumer:
            assert frame.timestamp_us == 0
            assert len(frame.payload) > 0
            break

        break


async def test_multiple_frames_ordering():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_media("opus", opus_head())
    _announce = origin.announce("ordering-test", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        catalog = await announcement.broadcast.catalog()
        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        media_consumer = await announcement.broadcast.subscribe_media(track_name, audio)

        timestamps = [0, 20_000, 40_000, 60_000, 80_000]
        for i, ts in enumerate(timestamps):
            media.write_frame(f"frame-{i}".encode(), ts)

        for i, expected_ts in enumerate(timestamps):
            async for frame in media_consumer:
                assert frame.timestamp_us == expected_ts
                assert frame.payload == f"frame-{i}".encode()
                break

        break


async def test_catalog_update_on_new_track():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    _media1 = broadcast.publish_media("opus", opus_head())
    _announce = origin.announce("catalog-update", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        cat_consumer = await announcement.broadcast.subscribe_catalog()

        # First catalog: 1 audio track.
        catalog1 = await anext(cat_consumer)
        assert len(catalog1.audio) == 1

        # Add a second audio track, which triggers a catalog update.
        _media2 = broadcast.publish_media("opus", opus_head())

        catalog2 = await anext(cat_consumer)
        assert len(catalog2.audio) == 2

        break


def test_finish_closes_producer():
    broadcast = moq.BroadcastProducer()
    _media = broadcast.publish_media("opus", opus_head())
    broadcast.finish()

    with pytest.raises(Exception):
        broadcast.finish()


async def test_announced_broadcast():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    _announce = origin.announce("test/broadcast", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.path == "test/broadcast"
        _catalog = await announcement.broadcast.subscribe_catalog()
        break


def test_publish_lifecycle():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    track.write_frame(b'{"cmd": "ready"}', 0)
    track.finish()
    broadcast.finish()


async def test_publish_track_info_and_subscription():
    """Raw track published with explicit TrackInfo, consumed with a Subscription."""
    broadcast = moq.BroadcastProducer()
    info = moq.TrackInfo(priority=5, latency_max_ms=2_000)
    track = broadcast.publish_track("status", info)

    consumer = track.consume(moq.Subscription(priority=3))
    track.write_frame(b"ready", 0)

    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == b"ready"
    track.finish()


async def test_fetch_group_and_serve_dynamic_miss():
    """Fetch a cached group, then serve an uncached sequence through TrackDynamic."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("events")
    consumer = broadcast.consume()

    cached = track.append_group()
    cached.write_frame(b"cached", 0)
    cached.finish()

    fetched = await consumer.fetch_group("events", 0, moq.FetchGroupOptions(priority=3))
    assert fetched.sequence == 0
    assert [frame.payload async for frame in fetched] == [b"cached"]

    dynamic = track.dynamic()
    pending = asyncio.create_task(consumer.fetch_group("events", 7, moq.FetchGroupOptions(priority=11)))
    request = await asyncio.wait_for(dynamic.requested_group(), timeout=5.0)
    assert request.sequence == 7
    assert request.priority == 11

    produced = request.accept()
    produced.write_frame(b"archive", 140_000)
    produced.finish()

    fetched = await asyncio.wait_for(pending, timeout=5.0)
    assert [frame.payload async for frame in fetched] == [b"archive"]


async def test_json_snapshot_roundtrip():
    broadcast = moq.BroadcastProducer()
    producer = broadcast.publish_json_snapshot("status", compression=True)
    consumer = await broadcast.consume().subscribe_json_snapshot("status", compression=True)

    producer.update({"state": "live", "viewers": 1})
    value = await asyncio.wait_for(anext(consumer), timeout=5.0)
    assert value == {"state": "live", "viewers": 1}

    producer.update({"state": "live", "viewers": 2})
    value = await asyncio.wait_for(anext(consumer), timeout=5.0)
    assert value == {"state": "live", "viewers": 2}

    producer.finish()


async def test_json_stream_roundtrip():
    broadcast = moq.BroadcastProducer()
    producer = broadcast.publish_json_stream("events")
    consumer = await broadcast.consume().subscribe_json_stream("events")

    for n in range(3):
        producer.append({"n": n})
        record = await asyncio.wait_for(anext(consumer), timeout=5.0)
        assert record == {"n": n}

    producer.finish()


async def test_dynamic_track_request():
    broadcast = moq.BroadcastProducer()
    dynamic = broadcast.dynamic()
    consumer = broadcast.consume()

    # The subscribe stays pending until the request is accepted below; run it concurrently.
    subscribe = asyncio.create_task(consumer.subscribe_track("events"))

    request = await asyncio.wait_for(dynamic.requested_track(), timeout=5.0)
    assert request.name == "events"

    # Accept the request as a raw track (which unblocks the subscribe), then write.
    track = request.accept()
    payload = b"hello dynamic track"
    track.write_frame(payload, 0)

    track_consumer = await asyncio.wait_for(subscribe, timeout=5.0)
    frame = await asyncio.wait_for(track_consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == payload

    track.finish()


async def test_dynamic_track_request_can_publish_media():
    broadcast = moq.BroadcastProducer()
    dynamic = broadcast.dynamic()
    consumer = broadcast.consume()
    catalog_consumer = await consumer.subscribe_catalog()

    # publish_media_on_track accepts the request (at the media timescale), which is what
    # unblocks subscribe_media, so run the subscribe concurrently until then.
    subscribe = asyncio.create_task(
        consumer.subscribe_media("requested-audio", cast(moq.Container, moq.Container.LEGACY()))
    )

    track = await asyncio.wait_for(dynamic.requested_track(), timeout=5.0)
    assert track.name == "requested-audio"

    media = broadcast.publish_media_on_track(track, "opus", opus_head())
    assert media.name == "requested-audio"
    with pytest.raises(Exception):
        _ = track.name

    media_consumer = await asyncio.wait_for(subscribe, timeout=5.0)

    catalog = await asyncio.wait_for(anext(catalog_consumer), timeout=5.0)
    audio = catalog.audio["requested-audio"]
    assert audio.codec == "opus"
    assert audio.sample_rate == 48000
    assert audio.channel_count == 2

    payload = b"dynamic opus frame"
    media.write_frame(payload, 20_000)

    async for frame in media_consumer:
        assert frame.payload == payload
        assert frame.timestamp_us == 20_000
        break

    media.finish()


async def test_dynamic_broadcast_request():
    origin = moq.OriginProducer(cache_capacity_bytes=4096)
    dynamic = origin.dynamic()
    consumer = origin.consume()

    request_broadcast = asyncio.create_task(consumer.request_broadcast("dynamic/broadcast"))

    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "dynamic/broadcast"

    served = moq.BroadcastProducer()
    track = served.publish_track("status")
    request.accept(served)
    with pytest.raises(Exception):
        _ = request.path

    broadcast = await asyncio.wait_for(request_broadcast, timeout=5.0)
    track_consumer = await broadcast.subscribe_track("status")
    payload = b"served dynamically"
    track.write_frame(payload, 20_000)

    frame = await asyncio.wait_for(track_consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == payload
    assert frame.timestamp_us == 20_000
    track.finish()
    served.finish()


async def test_dynamic_broadcast_request_can_reject():
    origin = moq.OriginProducer()
    dynamic = origin.dynamic()
    consumer = origin.consume()

    request_broadcast = asyncio.create_task(consumer.request_broadcast("missing"))
    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "missing"

    request.abort(404)
    with pytest.raises(Exception):
        _ = request.path

    with pytest.raises(Exception):
        await asyncio.wait_for(request_broadcast, timeout=5.0)


def test_raw_append_group_sequence_increments():
    """append_group hands out monotonically increasing sequence numbers."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("seq")

    sequences = []
    for _ in range(5):
        group = track.append_group()
        sequences.append(group.sequence)
        group.finish()

    assert sequences == [0, 1, 2, 3, 4]


def test_raw_group_write_multiple_frames():
    """A single group accepts multiple write_frame calls before finish."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("chunks")

    group = track.append_group()
    for i in range(10):
        group.write_frame(f"frame-{i}".encode(), i)
    group.finish()


def test_raw_group_empty_payload():
    """Empty frames are a valid payload."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("empty")

    group = track.append_group()
    group.write_frame(b"", 0)
    group.finish()


def test_raw_group_write_after_finish_fails():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("t")
    group = track.append_group()
    group.finish()

    with pytest.raises(Exception):
        group.write_frame(b"too late", 0)


def test_raw_group_finish_twice_fails():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("t")
    group = track.append_group()
    group.finish()

    with pytest.raises(Exception):
        group.finish()


def test_raw_track_write_after_finish_fails():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("t")
    track.finish()

    with pytest.raises(Exception):
        track.write_frame(b"late", 0)


def test_raw_sparse_groups_and_known_end():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("sparse")

    group = track.create_group(2)
    assert group.sequence == 2
    group.finish()

    track.finish_at(5)
    track.create_group(4).finish()
    with pytest.raises(Exception):
        track.create_group(5)
    track.finish()

    with pytest.raises(Exception):
        track.append_group()


def test_raw_parallel_groups():
    """Appending a new group before finishing the previous is allowed;
    both groups carry distinct sequences and can be written independently."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("parallel")

    g0 = track.append_group()
    g1 = track.append_group()
    assert g0.sequence == 0
    assert g1.sequence == 1

    g0.write_frame(b"a0", 0)
    g1.write_frame(b"b0", 0)
    g0.write_frame(b"a1", 1)
    g0.finish()
    g1.finish()


def test_public_api_exports():
    """The ergonomic surface is reachable from the top-level package, so users
    never have to import the private `moq._uniffi` module."""
    assert issubclass(moq.Error, Exception)
    # Flat-error variants are accessible as attributes for selective catching.
    assert hasattr(moq.Error, "AlreadyResponded")
    assert hasattr(moq.Error, "Cancelled")
    assert callable(moq.log_level)
    assert isinstance(moq.connect("https://example.com"), moq.Client)
    client = moq.connect(
        "https://example.com",
        tls_roots=["root.pem"],
        tls_fingerprints=["abc123"],
    )
    assert client._tls_roots == ["root.pem"]
    assert client._tls_fingerprints == ["abc123"]


async def test_subscribe_media_default_latency_and_context_manager():
    """subscribe_media takes the catalog record directly and defaults the
    latency; the returned consumer is also an async context manager."""
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_media("opus", opus_head())
    _announce = origin.announce("live", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        catalog = await announcement.broadcast.catalog()
        track_name, audio = next(iter(catalog.audio.items()))

        # No container argument, no explicit latency.
        payload = b"opus audio payload data"
        media.write_frame(payload, 1_000_000)

        async with await announcement.broadcast.subscribe_media(track_name, audio) as media_consumer:
            async for frame in media_consumer:
                assert frame.payload == payload
                break

        break


async def test_raw_publish_consume():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("events")
    _announce = origin.announce("robot/arm", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.path == "robot/arm"

        raw_consumer = await announcement.broadcast.subscribe_track("events")

        payload = b'{"cmd": "button_changed", "arm": "left", "button": "THUMB", "state": "PRESSED"}'
        raw.write_frame(payload, 0)

        async for group in raw_consumer:
            async for frame in group:
                assert frame.payload == payload
                break
            break

        break


async def test_raw_multiple_frames():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("commands")
    _announce = origin.announce("robot/io", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        raw_consumer = await announcement.broadcast.subscribe_track("commands")

        messages = [
            b'{"cmd": "led", "arm": "left", "led": "THUMB", "state": 1}',
            b'{"cmd": "tone", "arm": "right", "freq": 440}',
            b'{"cmd": "tone_stop", "arm": "right"}',
        ]
        for msg in messages:
            raw.write_frame(msg, 0)

        received = []
        async for group in raw_consumer:
            async for frame in group:
                received.append(frame.payload)
            if len(received) == len(messages):
                break

        assert received == messages
        break


async def test_raw_producer_consume_direct():
    """Consume a raw track directly from the producer, no origin/broadcast plumbing."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("direct")
    consumer = track.consume()

    track.write_frame(b"hello", 0)
    track.write_frame(b"world", 0)

    received = []
    async for group in consumer:
        async for frame in group:
            received.append(frame.payload)
        if len(received) == 2:
            break

    assert received == [b"hello", b"world"]


async def test_raw_group_producer_consume_direct():
    """Consume a single group directly from the group producer."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("group-direct")
    group = track.append_group()
    group_consumer = group.consume()
    assert group_consumer.sequence == group.sequence

    group.write_frame(b"a", 0)
    group.write_frame(b"b", 0)
    group.finish()

    received = [frame.payload async for frame in group_consumer]
    assert received == [b"a", b"b"]


async def test_broadcast_producer_consume_direct():
    """Consume a broadcast directly from the producer with catalog and raw track."""
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("events")
    consumer = broadcast.consume()

    raw_consumer = await consumer.subscribe_track("events")
    raw.write_frame(b"event-0", 0)

    async for group in raw_consumer:
        async for frame in group:
            assert frame.payload == b"event-0"
            break
        break


async def test_raw_group_sequence():
    """Consumer sees the same sequence numbers the producer assigned."""
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("seq")
    _announce = origin.announce("track/seq", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        raw_consumer = await announcement.broadcast.subscribe_track("seq")

        sent_sequences = []
        for i in range(3):
            group = raw.append_group()
            sent_sequences.append(group.sequence)
            group.write_frame(f"msg-{i}".encode(), i)
            group.finish()

        received_sequences = []
        async for group in raw_consumer:
            received_sequences.append(group.sequence)
            async for _ in group:
                pass
            if len(received_sequences) == len(sent_sequences):
                break

        assert received_sequences == sent_sequences
        break


async def test_default_iteration_is_sequence_order():
    """Iterating a track yields sequence order; groups_as_arrived yields arrival order.

    Group 5 is produced before group 3, so the two orderings genuinely diverge and
    this fails if the default iteration ever reverts to recv_group.
    """
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("ordering")
    _announce = origin.announce("track/ordering", broadcast)

    sequenced = origin.consume()
    arrived = origin.consume()

    seq_consumer = await (await anext(sequenced.announced())).broadcast.subscribe_track("ordering")
    arr_consumer = await (await anext(arrived.announced())).broadcast.subscribe_track("ordering")

    for sequence in (5, 3):
        group = raw.create_group(sequence)
        group.write_frame(f"group-{sequence}".encode(), 0)
        group.finish()

    # Arrival order sees them as produced, newest sequence first.
    assert [g.sequence async for g in _take(arr_consumer.groups_as_arrived(), 2)] == [5, 3]

    # The default iteration sorts them back into ascending sequence order.
    assert [g.sequence async for g in _take(seq_consumer, 2)] == [3, 5]


async def _take(iterator, count: int):
    """Yield the first `count` items of an async iterator."""
    taken = 0
    async for item in iterator:
        yield item
        taken += 1
        if taken == count:
            return


async def test_raw_multi_frame_group():
    """A single group can carry multiple frames, not just one per group."""
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("chunks")
    _announce = origin.announce("stream/chunks", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        raw_consumer = await announcement.broadcast.subscribe_track("chunks")

        group_producer = raw.append_group()
        chunks = [b"chunk-0", b"chunk-1", b"chunk-2"]
        for chunk in chunks:
            group_producer.write_frame(chunk, 0)
        group_producer.finish()

        async for group in raw_consumer:
            received = [frame.payload async for frame in group]
            assert received == chunks
            break

        break


async def test_read_frame_one_per_group():
    """read_frame() returns the first frame of each successive group."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    consumer = track.consume()

    track.write_frame(b"ready", 0)
    track.write_frame(b"running", 0)
    track.write_frame(b"done", 0)

    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"ready"
    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"running"
    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"done"


async def test_raw_read_frame_preserves_timestamp():
    """read_frame() returns raw payloads with their presentation timestamp."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    consumer = track.consume()

    track.write_frame(b"ready", 12_345)
    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"ready"
    assert frame.timestamp_us == 12_345

    group = track.append_group()
    group_consumer = group.consume()
    group.write_frame(b"group", 23_456)
    group.finish()

    frame = await group_consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"group"
    assert frame.timestamp_us == 23_456


async def test_read_frame_skips_remaining_frames_in_group():
    """read_frame() only returns the first frame of a multi-frame group."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("mixed")
    consumer = track.consume()

    group = track.append_group()
    group.write_frame(b"first", 0)
    group.write_frame(b"second-ignored", 0)
    group.finish()

    track.write_frame(b"next-group-first", 0)

    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"first"
    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"next-group-first"


async def test_read_frame_returns_none_when_track_finished():
    """read_frame() returns None once the producer finishes with no more groups."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("done")
    consumer = track.consume()

    track.write_frame(b"only", 0)
    track.finish()

    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"only"
    assert await consumer.read_frame() is None

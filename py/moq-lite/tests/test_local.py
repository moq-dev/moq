"""Local pub/sub tests — no network required."""

import struct

import moq_lite as moq
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
    origin.publish("live", broadcast)

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

        media_consumer = announcement.broadcast.subscribe_media(track_name, audio.container, 10_000)

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
    origin.publish("video-test", broadcast)

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

        media_consumer = announcement.broadcast.subscribe_media(track_name, video.container, 10_000)

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
    origin.publish("ordering-test", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        catalog = await announcement.broadcast.catalog()
        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        media_consumer = announcement.broadcast.subscribe_media(track_name, audio.container, 10_000)

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
    origin.publish("catalog-update", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        cat_consumer = announcement.broadcast.subscribe_catalog()

        # First catalog: 1 audio track.
        catalog1 = await anext(cat_consumer)
        assert len(catalog1.audio) == 1

        # Add a second audio track — triggers catalog update.
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
    origin.publish("test/broadcast", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.path == "test/broadcast"
        _catalog = announcement.broadcast.subscribe_catalog()
        break


def test_publish_lifecycle():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    track.write_frame(b'{"cmd": "ready"}')
    track.finish()
    broadcast.finish()


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
        group.write_frame(f"frame-{i}".encode())
    group.finish()


def test_raw_group_empty_payload():
    """Empty frames are a valid payload."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("empty")

    group = track.append_group()
    group.write_frame(b"")
    group.finish()


def test_raw_group_write_after_finish_fails():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("t")
    group = track.append_group()
    group.finish()

    with pytest.raises(Exception):
        group.write_frame(b"too late")


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
        track.write_frame(b"late")

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

    g0.write_frame(b"a0")
    g1.write_frame(b"b0")
    g0.write_frame(b"a1")
    g0.finish()
    g1.finish()


async def test_raw_publish_consume():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("events")
    origin.publish("robot/arm", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.path == "robot/arm"

        raw_consumer = announcement.broadcast.subscribe_track("events")

        payload = b'{"cmd": "button_changed", "arm": "left", "button": "THUMB", "state": "PRESSED"}'
        raw.write_frame(payload)

        async for group in raw_consumer:
            async for frame in group:
                assert frame == payload
                break
            break

        break


async def test_raw_multiple_frames():
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("commands")
    origin.publish("robot/io", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        raw_consumer = announcement.broadcast.subscribe_track("commands")

        messages = [
            b'{"cmd": "led", "arm": "left", "led": "THUMB", "state": 1}',
            b'{"cmd": "tone", "arm": "right", "freq": 440}',
            b'{"cmd": "tone_stop", "arm": "right"}',
        ]
        for msg in messages:
            raw.write_frame(msg)

        received = []
        async for group in raw_consumer:
            async for frame in group:
                received.append(frame)
            if len(received) == len(messages):
                break

        assert received == messages
        break


async def test_raw_producer_consume_direct():
    """Consume a raw track directly from the producer, no origin/broadcast plumbing."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("direct")
    consumer = track.consume()

    track.write_frame(b"hello")
    track.write_frame(b"world")

    received = []
    async for group in consumer:
        async for frame in group:
            received.append(frame)
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

    group.write_frame(b"a")
    group.write_frame(b"b")
    group.finish()

    received = [frame async for frame in group_consumer]
    assert received == [b"a", b"b"]


async def test_broadcast_producer_consume_direct():
    """Consume a broadcast directly from the producer — catalog + raw track."""
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("events")
    consumer = broadcast.consume()

    raw_consumer = consumer.subscribe_track("events")
    raw.write_frame(b"event-0")

    async for group in raw_consumer:
        async for frame in group:
            assert frame == b"event-0"
            break
        break


async def test_raw_group_sequence():
    """Consumer sees the same sequence numbers the producer assigned."""
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("seq")
    origin.publish("track/seq", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        raw_consumer = announcement.broadcast.subscribe_track("seq")

        sent_sequences = []
        for i in range(3):
            group = raw.append_group()
            sent_sequences.append(group.sequence)
            group.write_frame(f"msg-{i}".encode())
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


async def test_raw_multi_frame_group():
    """A single group can carry multiple frames — not just one-per-group."""
    origin = moq.OriginProducer()
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("chunks")
    origin.publish("stream/chunks", broadcast)

    consumer = origin.consume()

    async for announcement in consumer.announced():
        raw_consumer = announcement.broadcast.subscribe_track("chunks")

        group_producer = raw.append_group()
        chunks = [b"chunk-0", b"chunk-1", b"chunk-2"]
        for chunk in chunks:
            group_producer.write_frame(chunk)
        group_producer.finish()

        async for group in raw_consumer:
            received = [frame async for frame in group]
            assert received == chunks
            break

        break


async def test_read_frame_one_per_group():
    """read_frame() returns the first frame of each successive group."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    consumer = track.consume()

    track.write_frame(b"ready")
    track.write_frame(b"running")
    track.write_frame(b"done")

    assert await consumer.read_frame() == b"ready"
    assert await consumer.read_frame() == b"running"
    assert await consumer.read_frame() == b"done"


async def test_read_frame_skips_remaining_frames_in_group():
    """read_frame() only returns the first frame of a multi-frame group."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("mixed")
    consumer = track.consume()

    group = track.append_group()
    group.write_frame(b"first")
    group.write_frame(b"second-ignored")
    group.finish()

    track.write_frame(b"next-group-first")

    assert await consumer.read_frame() == b"first"
    assert await consumer.read_frame() == b"next-group-first"


async def test_read_frame_returns_none_when_track_finished():
    """read_frame() returns None once the producer finishes with no more groups."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("done")
    consumer = track.consume()

    track.write_frame(b"only")
    track.finish()

    assert await consumer.read_frame() == b"only"
    assert await consumer.read_frame() is None

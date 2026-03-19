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

        media_consumer = announcement.broadcast.subscribe_media(track_name)

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

        media_consumer = announcement.broadcast.subscribe_media(track_name)

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
        media_consumer = announcement.broadcast.subscribe_media(track_name)

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

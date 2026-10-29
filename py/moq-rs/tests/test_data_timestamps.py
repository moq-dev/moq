"""Publishers preserve the caller's exact capture time, including absence."""

import asyncio
import json
import struct
from datetime import timedelta

import moq
import moq_ffi
import pytest

TIMESTAMPS = [None, moq.Timestamp(0, 90_000), moq.Timestamp(1, 90_000)]


async def test_raw_timestamps_are_exact():
    """A present zero stays present, and one 90 kHz tick reads back as itself, not the 11 µs it floors to."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("video", moq.TrackInfo(timescale=90_000))
    consumer = track.consume()
    for value in [0, 1]:
        track.write_frame(b"frame", moq.Timestamp(value, 90_000))
        frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
        assert frame is not None
        assert frame.timestamp == moq.Timestamp(value, 90_000)

    untimed = broadcast.publish_track("events", moq.TrackInfo(timescale=None))
    untimed_consumer = untimed.consume()
    untimed.write_frame(b"event")
    frame = await asyncio.wait_for(untimed_consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.timestamp is None

    for invalid in [moq.Timestamp(0, 0), moq.Timestamp(0, 1 << 62), moq.Timestamp(1 << 61, 90_000)]:
        with pytest.raises(moq.Error.TimeOverflow):
            track.write_frame(b"frame", invalid)
    broadcast.close()


def test_timedelta_conversions():
    assert moq.Timestamp.from_timedelta(timedelta(seconds=1, microseconds=5)) == moq.Timestamp(1_000_005, 1_000_000)
    with pytest.raises(ValueError):
        moq.Timestamp.from_timedelta(timedelta(microseconds=-1))
    # Rounded down, like moq-net: half a microsecond and 2/3 of one are both 0, and 11.1 µs is 11 µs.
    assert moq.Timestamp(1, 2_000_000).to_timedelta() == timedelta(0)
    assert moq.Timestamp(2, 3_000_000).to_timedelta() == timedelta(0)
    assert moq.Timestamp(1, 90_000).to_timedelta() == timedelta(microseconds=11)


@pytest.mark.parametrize("mode", ["snapshot", "stream"])
@pytest.mark.parametrize("timestamp", TIMESTAMPS)
async def test_json_timestamp_round_trip(mode: str, timestamp: moq.Timestamp | None):
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("value", moq.TrackInfo(timescale=None if timestamp is None else timestamp.scale))
    consumer = track.consume()
    value = {"n": 1}
    if mode == "snapshot":
        snapshot = moq.json.SnapshotProducer(broadcast, track)
        snapshot.update(value, timestamp=timestamp)
        snapshot.finish()
    else:
        stream = moq.json.StreamProducer(broadcast, track)
        stream.append(value, timestamp=timestamp)
        stream.finish()
    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert json.loads(frame.payload) == value
    assert frame.timestamp == timestamp
    broadcast.close()


def test_catalog_now_uses_a_weak_broadcast_handle():
    broadcast = moq.BroadcastProducer()
    catalog = moq.media.CatalogProducer(broadcast)
    assert catalog.now().scale == 1_000_000
    broadcast.close()
    with pytest.raises(moq.Error.Closed):
        catalog.now()


def test_whole_media_frames_require_the_callers_timestamp():
    broadcast = moq.BroadcastProducer()
    head = (
        b"OpusHead"
        + bytes([1, 2])
        + struct.pack("<H", 0)
        + struct.pack("<I", 48000)
        + struct.pack("<H", 0)
        + bytes([0])
    )
    producer = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=head)
    )
    with pytest.raises(moq.Error.Codec, match="timestamp"):
        producer._inner.write_frame(moq_ffi.MoqFrame(payload=b"opus frame", timestamp=None))
    # Exercise the runtime refusal when a caller omits the required timestamp.
    with pytest.raises(TypeError):
        producer.write_frame(b"opus frame")  # type: ignore[call-arg]
    producer.finish()
    broadcast.close()

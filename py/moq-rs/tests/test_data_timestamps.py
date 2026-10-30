"""Data publishers preserve the caller's capture time, including absence."""

import asyncio
import json
import struct
from datetime import timedelta

import moq
import moq_ffi
import pytest


@pytest.mark.parametrize("mode", ["snapshot", "stream"])
@pytest.mark.parametrize("timestamp", [None, timedelta(microseconds=123_456)])
async def test_json_timestamp_round_trip(mode: str, timestamp: timedelta | None):
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("value", moq.TrackInfo(timescale=None if timestamp is None else 1_000_000))
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
    assert catalog.now() >= timedelta(0)
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
        producer._inner.write_frame(moq_ffi.MoqFrame(payload=b"opus frame", timestamp_us=None))
    # Exercise the runtime refusal when a caller omits the required timestamp.
    with pytest.raises(TypeError):
        producer.write_frame(b"opus frame")  # type: ignore[call-arg]
    producer.finish()
    broadcast.close()

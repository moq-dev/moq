"""Local pub/sub tests: no network required."""

import asyncio
import struct
from typing import cast

import moq
import pytest


def create_announced(origin: moq.OriginProducer, path: str) -> moq.BroadcastProducer:
    broadcast = origin.create_broadcast(path)
    broadcast.announce()
    return broadcast


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


async def test_fetch_abort_is_a_stream_app_code():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("events")
    dynamic = track.dynamic()
    consumer = broadcast.consume()

    async def reject():
        request = await dynamic.requested_group()
        request.abort(404)

    task = asyncio.create_task(reject())
    with pytest.raises(moq.Error.Protocol) as raised:  # type: ignore[attr-defined]
        await consumer.fetch_group("events", 5)
    await task
    protocol = moq.protocol_error(raised.value)
    assert protocol is not None
    assert protocol.scope == moq.ErrorScope.STREAM
    assert protocol.code == 64 + 404
    assert protocol.kind == moq.ProtocolKind.APP


def test_protocol_error_helper_covers_known_app_and_unknown():
    cases = [
        (moq.ErrorScope.SESSION, 0x2, moq.ProtocolKind.UNAUTHORIZED, True),
        (moq.ErrorScope.SESSION, 64 + 404, moq.ProtocolKind.APP, False),
        (moq.ErrorScope.SESSION, 0x1F, moq.ProtocolKind.UNKNOWN, False),
        (moq.ErrorScope.STREAM, 64 + 7, moq.ProtocolKind.APP, False),
    ]
    for scope, code, kind, auth in cases:
        details = moq.ProtocolError(scope=scope, code=code, kind=kind, message="x")
        err = moq.Error.Protocol(details)
        protocol = moq.protocol_error(err)
        assert protocol is not None
        assert protocol.scope == scope
        assert protocol.code == code
        assert protocol.kind == kind
        assert moq.is_auth(err) is auth
    assert moq.protocol_error(RuntimeError("nope")) is None


def test_publish_media_lifecycle():
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())
    media.write_frame(b"opus frame", 1000)
    media.finish()
    broadcast.close()


def test_publish_media_cut_and_seek():
    """Audio has no keyframes, so `cut` is the only thing that bounds its groups.

    Without it every packet lands in one group that never closes, which strands
    late subscribers and the timeline alike.
    """
    broadcast = moq.BroadcastProducer()
    media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

    for i in range(3):
        media.write_frame(b"opus frame", i * 20_000)
        media.cut()

    # The same boundary, with the next group explicitly numbered.
    media.write_frame(b"opus frame", 60_000)
    media.seek(42)

    media.finish()
    broadcast.close()


def test_video_properties_use_defaulted_fields():
    broadcast = moq.BroadcastProducer()
    properties = moq.VideoProperties(rotation=315.0)
    assert properties.display is None
    assert properties.flip is None
    broadcast.set_video_properties(properties)
    broadcast.close()


def test_audio_rejects_bad_init_bytes():
    # A bad format is no longer expressible: it is an enum. What can still go wrong here is
    # init bytes that aren't an OpusHead, which must fail at publish rather than on a frame.
    broadcast = moq.BroadcastProducer()
    with pytest.raises(Exception):
        broadcast.publish_audio(moq.AudioFormat.OPUS, b"")


async def test_local_publish_consume_audio():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "live")
    media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.prefix == "live"

        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        catalog = await broadcast_consumer.catalog()

        assert len(catalog.audio) == 1
        assert len(catalog.video) == 0

        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        assert audio.codec == "opus"
        assert audio.sample_rate == 48000
        assert audio.channel_count == 2

        media_consumer = await broadcast_consumer.subscribe_media(track_name, audio)

        payload = b"opus audio payload data"
        media.write_frame(payload, 1_000_000)

        async for frame in media_consumer:
            assert frame.payload == payload
            assert frame.timestamp_us == 1_000_000
            break

        break


async def test_video_publish_consume():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "video-test")
    media = broadcast.publish_video(moq.VideoFormat.AVC3, h264_init())

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        catalog = await broadcast_consumer.catalog()

        assert len(catalog.video) == 1
        assert len(catalog.audio) == 0

        track_name = list(catalog.video.keys())[0]
        video = catalog.video[track_name]
        assert video.codec.startswith("avc1.") or video.codec.startswith("avc3.")
        assert video.coded is not None
        assert video.coded.width == 1280
        assert video.coded.height == 720

        media_consumer = await broadcast_consumer.subscribe_media(track_name, video)

        keyframe = bytes([0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC])
        media.write_frame(keyframe, 0)

        async for frame in media_consumer:
            assert frame.timestamp_us == 0
            assert len(frame.payload) > 0
            break

        break


async def test_video_publish_named_track():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "video-named-test")
    media = broadcast.publish_video(moq.VideoFormat.AVC3, h264_init(), track="hd")
    assert media.name == "hd"

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        catalog = await broadcast_consumer.catalog()
        assert list(catalog.video.keys()) == ["hd"]
        break


async def test_multiple_frames_ordering():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "ordering-test")
    media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        catalog = await broadcast_consumer.catalog()
        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        media_consumer = await broadcast_consumer.subscribe_media(track_name, audio)

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
    broadcast = create_announced(origin, "catalog-update")
    _media1 = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        cat_consumer = await broadcast_consumer.subscribe_catalog()

        # First catalog: 1 audio track.
        catalog1 = await anext(cat_consumer)
        assert len(catalog1.audio) == 1

        # Add a second audio track, which triggers a catalog update.
        _media2 = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

        catalog2 = await anext(cat_consumer)
        assert len(catalog2.audio) == 2

        break


def test_close_twice_is_a_noop():
    broadcast = moq.BroadcastProducer()
    _media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())
    broadcast.close()
    broadcast.close()

    with pytest.raises(Exception):
        broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())


def test_finish_is_deprecated():
    broadcast = moq.BroadcastProducer()
    with pytest.deprecated_call():
        broadcast.finish()


async def test_announced_broadcast():
    origin = moq.OriginProducer()
    _broadcast = create_announced(origin, "test/broadcast")

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.prefix == "test/broadcast"
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        _catalog = await broadcast_consumer.subscribe_catalog()
        break


def test_publish_lifecycle():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    track.write_frame(b'{"cmd": "ready"}', 0)
    track.finish()
    broadcast.close()


async def test_publish_track_info_and_subscription():
    """Raw track published with explicit TrackInfo, consumed with a Subscription."""
    broadcast = moq.BroadcastProducer()
    info = moq.TrackInfo(priority=5, max_age_us=2_000_000)
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


async def test_json_producers_report_demand():
    broadcast = moq.BroadcastProducer()
    snapshot = broadcast.publish_json_snapshot("status", compression=True)
    stream = broadcast.publish_json_stream("events")
    snapshot_demand = snapshot.demand()
    stream_demand = stream.demand()
    assert snapshot_demand.name == "status"
    assert not snapshot_demand.is_used()

    consumer = broadcast.consume()
    snapshot_consumer = await consumer.subscribe_json_snapshot("status", compression=True)
    stream_consumer = await consumer.subscribe_json_stream("events")
    await asyncio.wait_for(snapshot_demand.used(), timeout=5.0)
    await asyncio.wait_for(stream_demand.used(), timeout=5.0)
    assert snapshot_demand.is_used()

    snapshot_consumer.cancel()
    stream_consumer.cancel()
    await asyncio.wait_for(snapshot_demand.unused(), timeout=5.0)
    await asyncio.wait_for(stream_demand.unused(), timeout=5.0)

    snapshot.finish()
    with pytest.raises(moq.Error.Closed):  # type: ignore[attr-defined]
        await asyncio.wait_for(snapshot_demand.used(), timeout=5.0)


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

    # publish_audio_on_track accepts the request (at the media timescale), which is what
    # unblocks subscribe_media, so run the subscribe concurrently until then.
    subscribe = asyncio.create_task(
        consumer.subscribe_media("requested-audio", cast(moq.Container, moq.Container.LEGACY()))
    )

    track = await asyncio.wait_for(dynamic.requested_track(), timeout=5.0)
    assert track.name == "requested-audio"

    media = broadcast.publish_audio_on_track(track, moq.AudioFormat.OPUS, opus_head())
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
    dynamic = origin.dynamic("")
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
    served.close()


async def test_dynamic_broadcast_request_can_reject():
    origin = moq.OriginProducer()
    dynamic = origin.dynamic("")
    consumer = origin.consume()

    request_broadcast = asyncio.create_task(consumer.request_broadcast("missing"))
    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "missing"

    request.reject(404)
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


def test_raw_group_abort_after_finish():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("t")
    group = track.append_group()
    group.finish()
    group.abort(409)


def test_raw_track_abort_after_finish():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("t")
    track.finish()
    track.abort(409)


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
    assert hasattr(moq.Error, "Busy")
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
    broadcast = create_announced(origin, "live")
    media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head())

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        catalog = await broadcast_consumer.catalog()
        track_name, audio = next(iter(catalog.audio.items()))

        # No container argument, no explicit latency.
        payload = b"opus audio payload data"
        media.write_frame(payload, 1_000_000)

        async with await broadcast_consumer.subscribe_media(track_name, audio) as media_consumer:
            async for frame in media_consumer:
                assert frame.payload == payload
                break

        break


async def test_raw_publish_consume():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "robot/arm")
    raw = broadcast.publish_track("events")

    consumer = origin.consume()

    async for announcement in consumer.announced():
        assert announcement.prefix == "robot/arm"

        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track("events")

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
    broadcast = create_announced(origin, "robot/io")
    raw = broadcast.publish_track("commands")

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track("commands", moq.Subscription(max_age_us=1_000_000))

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
    consumer = track.consume(moq.Subscription(max_age_us=1_000_000))

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
    broadcast = create_announced(origin, "track/seq")
    raw = broadcast.publish_track("seq")

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track("seq", moq.Subscription(max_age_us=1_000_000))

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
    broadcast = create_announced(origin, "track/ordering")
    raw = broadcast.publish_track("ordering")

    subscription = moq.Subscription(max_age_us=1_000_000)
    seq_consumer = raw.consume(subscription)
    arr_consumer = raw.consume(subscription)

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
    broadcast = create_announced(origin, "stream/chunks")
    raw = broadcast.publish_track("chunks")

    consumer = origin.consume()

    async for announcement in consumer.announced():
        broadcast_consumer = await consumer.request_broadcast(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track("chunks")

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
    consumer = track.consume(moq.Subscription(max_age_us=1_000_000))

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
    consumer = track.consume(moq.Subscription(max_age_us=1_000_000))

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


async def test_read_frame_skips_empty_group_on_open_track():
    """An empty completed group is not track EOF while the track is still open."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    consumer = track.consume()

    track.append_group().finish()

    read = asyncio.create_task(consumer.read_frame())
    await asyncio.sleep(0.05)
    assert not read.done(), "empty group must not end an open track"

    track.write_frame(b"after-empty", 1_000)
    frame = await asyncio.wait_for(read, timeout=5.0)
    assert frame is not None
    assert frame.payload == b"after-empty"
    assert frame.timestamp_us == 1_000


async def test_read_frame_skips_empty_then_populated_groups():
    """read_frame() walks past completed empty groups to the next first frame."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    consumer = track.consume()

    track.append_group().finish()
    track.append_group().finish()
    track.write_frame(b"populated", 2_000)

    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == b"populated"
    assert frame.timestamp_us == 2_000


async def test_read_frame_keeps_group_across_cancelled_call():
    """Cancelling one read_frame() must not drop the group it already acquired."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    consumer = track.consume()

    group = track.append_group()
    read = asyncio.create_task(consumer.read_frame())
    await asyncio.sleep(0.05)
    assert not read.done()
    read.cancel()
    with pytest.raises(asyncio.CancelledError):
        await read

    group.write_frame(b"kept", 3_000)
    group.finish()
    track.finish()

    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == b"kept"
    assert frame.timestamp_us == 3_000
    assert await asyncio.wait_for(consumer.read_frame(), timeout=5.0) is None


def test_optional_binding_records_use_none_defaults():
    """Optional UniFFI record fields are optional in generated constructors."""
    hint = moq.VideoHint()
    assert hint.coded is None
    assert hint.display_aspect is None
    assert hint.bitrate is None
    assert hint.framerate is None
    assert hint.optimize_for_latency is None

    encoder = moq.AudioEncoderOutput(codec=moq.AudioCodec.opus())
    assert encoder.sample_rate is None
    assert encoder.channels is None
    assert encoder.bitrate is None
    assert encoder.frame_duration_us == 20_000

    # Microseconds, so Opus' 2.5 ms frame is expressible at all.
    fine = moq.AudioEncoderOutput(codec=moq.AudioCodec.opus(), frame_duration_us=2_500)
    assert fine.frame_duration_us == 2_500

    decoder = moq.AudioDecoderOutput(format=moq.AudioSampleFormat.F32)
    assert decoder.sample_rate is None
    assert decoder.channels is None
    assert decoder.max_age_us is None


def test_encode_audio_with_opus_object():
    """Construct the Opus selection, encode actual audio, release in either order."""
    for codec_first in (True, False):
        broadcast = moq.BroadcastProducer()
        codec = moq.AudioCodec.opus()
        output = moq.AudioEncoderOutput(codec=codec)
        producer = broadcast.encode_audio(
            "mic",
            moq.AudioEncoderInput(format=moq.AudioSampleFormat.F32, sample_rate=48_000, channels=1),
            output,
        )
        if codec_first:
            del codec
            del output
        else:
            del output
            del codec
        # One 20 ms Opus frame of silence at 48 kHz mono.
        producer.write(moq.AudioFrame(timestamp_us=0, data=bytes(960 * 4)))
        assert producer.name == "mic"
        producer.finish()
        broadcast.close()


async def test_decode_video_frame():
    """A decoded frame owns its picture and converts to either layout, even after its consumer is cancelled."""
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "video-decode-frame")
    video = broadcast.encode_video(
        moq.VideoEncoderInput(format=moq.VideoPixelFormat.RGBA, width=320, height=240, framerate=30),
        # Software so the encode is deterministic everywhere. uniffi nests each
        # variant inside the enum without declaring it a subclass, so the cast is
        # what makes the variant typecheck.
        moq.VideoEncoderOutput(
            codec=moq.VideoCodec.H264,
            track="camera",
            kind=cast(moq.VideoEncoderKind, moq.VideoEncoderKind.SOFTWARE()),
        ),
    )

    # Seed the track so a subscriber joining below lands on encoded media.
    rgba = bytes([0x80]) * (320 * 240 * 4)
    video.cut()
    for i in range(10):
        video.write(moq.VideoFrame(timestamp_us=i * 33_333, data=rgba))

    consumer = origin.consume()
    broadcast_consumer = await asyncio.wait_for(consumer.request_broadcast("video-decode-frame"), timeout=5.0)
    catalog = await asyncio.wait_for(broadcast_consumer.catalog(), timeout=5.0)
    track_name = next(iter(catalog.video))
    rendition = catalog.video[track_name]

    decoder = await broadcast_consumer.decode_video(track_name, rendition)

    # Keep the encoder fed so the decoder sees frames after it joined.
    for i in range(10, 40):
        video.write(moq.VideoFrame(timestamp_us=i * 33_333, data=rgba))

    frame = await asyncio.wait_for(anext(decoder), timeout=5.0)
    decoder.cancel()

    i420 = frame.pixels(moq.VideoPixelFormat.I420)
    assert len(i420) == frame.width() * frame.height() * 3 // 2

    packed = frame.pixels(moq.VideoPixelFormat.RGBA)
    assert len(packed) == frame.width() * frame.height() * 4
    assert all(packed[i] == 0xFF for i in range(3, len(packed), 4)), "RGBA output should be opaque"

    video.finish()
    broadcast.close()


async def test_broadcast_is_reachable_only_while_announced():
    origin = moq.OriginProducer()
    broadcast = origin.create_broadcast("live")
    track = broadcast.publish_track("events")
    consumer = origin.consume()
    with pytest.raises(Exception):
        await asyncio.wait_for(consumer.request_broadcast("live"), timeout=5.0)

    broadcast.announce()
    announced = consumer.announced()
    first = await asyncio.wait_for(anext(announced), timeout=5.0)
    assert first.prefix == "live"
    assert first.active

    broadcast.unannounce()
    retracted = await asyncio.wait_for(anext(announced), timeout=5.0)
    assert retracted.prefix == "live"
    assert not retracted.active
    with pytest.raises(Exception):
        await asyncio.wait_for(consumer.request_broadcast("live"), timeout=5.0)

    broadcast.announce()
    back = await asyncio.wait_for(anext(announced), timeout=5.0)
    assert back.active
    await asyncio.wait_for(consumer.request_broadcast("live"), timeout=5.0)
    announced.cancel()
    track.finish()
    broadcast.close()


async def test_announced_pattern_captures():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    announced = consumer.announced("room", filter="*/chat")

    dynamic = origin.dynamic("room")
    overlap = await asyncio.wait_for(anext(announced), timeout=5.0)
    assert overlap.prefix == "room"
    assert overlap.captures is None

    audio = create_announced(origin, "room/alice/audio")
    chat = create_announced(origin, "room/alice/chat")
    match = await asyncio.wait_for(anext(announced), timeout=5.0)
    assert match.prefix == "room/alice/chat"
    assert match.captures == ["alice"]

    announced.cancel()
    dynamic.cancel()
    audio.close()
    chat.close()


async def test_dynamic_serves_a_request_under_a_prefix():
    origin = moq.OriginProducer()
    dynamic = origin.dynamic("live")
    consumer = origin.consume()

    pending = asyncio.create_task(consumer.request_broadcast("live/cam"))
    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "live/cam"
    served = moq.BroadcastProducer()
    request.accept(served)
    await asyncio.wait_for(pending, timeout=5.0)
    dynamic.cancel()
    served.close()


async def test_dynamic_and_json_handles_are_async_context_managers():
    """Every handle whose only cleanup is cancel() releases it on `async with` exit."""

    async def assert_cancelled(awaitable) -> None:
        with pytest.raises(Exception) as excinfo:
            await asyncio.wait_for(awaitable, timeout=5.0)
        assert moq.is_shutdown(excinfo.value)

    origin = moq.OriginProducer()
    async with origin.dynamic("live") as origin_dynamic:
        pass
    await assert_cancelled(origin_dynamic.requested_broadcast())

    broadcast = moq.BroadcastProducer()
    async with broadcast.dynamic() as broadcast_dynamic:
        pass
    await assert_cancelled(broadcast_dynamic.requested_track())

    track = broadcast.publish_track("events")
    async with track.dynamic() as track_dynamic:
        pass
    await assert_cancelled(track_dynamic.requested_group())

    snapshot = broadcast.publish_json_snapshot("state")
    async with await broadcast.consume().subscribe_json_snapshot("state") as snapshot_consumer:
        pass
    await assert_cancelled(anext(snapshot_consumer))

    stream = broadcast.publish_json_stream("log")
    async with await broadcast.consume().subscribe_json_stream("log") as stream_consumer:
        pass
    await assert_cancelled(anext(stream_consumer))

    snapshot.finish()
    stream.finish()
    track.finish()
    broadcast.close()

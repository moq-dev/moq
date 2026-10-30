"""Local pub/sub tests: no network required."""

import asyncio
import json
import struct
from dataclasses import replace
from datetime import timedelta
from typing import Any, cast

import moq
import moq_ffi
import pytest

# Every group from the first, rather than only what the live edge still reaches.
FROM_GROUP_0 = [moq.Range(start=cast(moq.Anchor, moq.Anchor.ABSOLUTE(group=0)))]


async def until_wanted(demand: moq.TrackDemand, wanted: bool = True) -> None:
    """Wait until some subscriber wants the track, or none does."""
    while (await demand.wanted_changed() is not None) != wanted:
        pass


def create_announced(origin: moq.OriginProducer, path: str) -> moq.BroadcastProducer:
    publication = origin.create_broadcast(path)
    publication.announce()
    return publication.broadcast


async def routes(announced: moq.AnnounceConsumer):
    """Yield each newly announced route, skipping updates and ends."""
    async for event in announced:
        if isinstance(event, moq.AnnounceEventStart):
            yield event.announce


async def next_route(announced: moq.AnnounceConsumer) -> moq.AnnounceEvent:
    """The next announce event."""
    return await asyncio.wait_for(anext(announced), timeout=5.0)


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


async def test_fetch_of_a_dropped_group_is_not_found():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("events")
    dynamic = track.requests()
    consumer = broadcast.consume()

    async def reject():
        fill = await dynamic.requested_fill()
        fill.drop_groups(5, 6)

    task = asyncio.create_task(reject())
    with pytest.raises(moq.Error.NotFound):
        await consumer.fetch_group("events", 5)
    await task


def test_protocol_error_helper_covers_known_app_and_unknown():
    cases = [
        (moq.ErrorScope.SESSION, 0x2, moq.ProtocolKind.UNAUTHORIZED, True),
        (moq.ErrorScope.SESSION, 0x18, moq.ProtocolKind.EXPIRED, True),
        (moq.ErrorScope.STREAM, 0x3B, moq.ProtocolKind.UNAUTHORIZED, True),
        (moq.ErrorScope.SESSION, 0x7, moq.ProtocolKind.TOO_MANY_REQUESTS, False),
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
    assert moq.is_auth(moq.Error.Expired())
    assert moq.protocol_error(RuntimeError("nope")) is None


def test_error_str_is_rust_display():
    assert str(moq.Error.Closed()) == "closed"
    error = moq.Error.Transport(reason="reset")
    assert str(error) == "transport: reset"
    details = moq.ProtocolError(scope=moq.ErrorScope.STREAM, code=468, kind=moq.ProtocolKind.APP, message="gone")
    assert str(moq.Error.Protocol(details)) == "gone"


def test_publish_media_lifecycle():
    broadcast = moq.BroadcastProducer()
    media = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )
    media.write_frame(b"opus frame", timedelta(microseconds=1000))
    media.finish()
    broadcast.close()


def test_publish_media_cut_and_seek():
    """Audio has no keyframes, so `cut` is the only thing that bounds its groups.

    Without it every packet lands in one group that never closes, which strands
    late subscribers and the timeline alike.
    """
    broadcast = moq.BroadcastProducer()
    media = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )

    for i in range(3):
        media.write_frame(b"opus frame", timedelta(microseconds=i * 20_000))
        media.cut()

    # The same boundary, with the next group explicitly numbered.
    media.write_frame(b"opus frame", timedelta(microseconds=60_000))
    media.seek(42)
    media.discontinuity()
    media.write_frame(b"resumed opus frame", timedelta(microseconds=100_000))

    media.finish()
    broadcast.close()


def test_video_properties_use_defaulted_fields():
    broadcast = moq.BroadcastProducer()
    properties = moq.media.VideoProperties(rotation=315.0)
    assert properties.display is None
    assert properties.flip is None
    moq.media.CatalogProducer(broadcast).set_video_properties(properties)
    broadcast.close()


def test_audio_rejects_bad_init_bytes():
    # A bad format is no longer expressible: it is an enum. What can still go wrong here is
    # init bytes that aren't an OpusHead, which must fail at publish rather than on a frame.
    broadcast = moq.BroadcastProducer()
    with pytest.raises(Exception):
        moq.media.TrackProducer.audio(broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=b""))


async def test_local_publish_consume_audio():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "live")
    media = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        assert announcement.prefix == "live"

        broadcast_consumer = await consumer.request(announcement.prefix)
        catalog = await moq.media.catalog(broadcast_consumer)

        assert len(catalog.audio) == 1
        assert len(catalog.video) == 0

        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        assert audio.codec == "opus"
        assert audio.sample_rate == 48000
        assert audio.channel_count == 2

        media_consumer = await moq.media.TrackConsumer.subscribe(broadcast_consumer, track_name, audio)

        payload = b"opus audio payload data"
        media.write_frame(payload, timedelta(microseconds=1_000_000))

        async for frame in media_consumer:
            assert isinstance(frame, moq.media.MediaFrame)
            assert frame.payload == payload
            assert frame.timestamp // timedelta(microseconds=1) == 1_000_000
            break

        break


async def test_video_publish_consume():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "video-test")
    media = moq.media.TrackProducer.video(
        broadcast, moq.media.VideoInit(format=moq.media.VideoFormat.AVC3, data=h264_init())
    )

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        catalog = await moq.media.catalog(broadcast_consumer)

        assert len(catalog.video) == 1
        assert len(catalog.audio) == 0

        track_name = list(catalog.video.keys())[0]
        video = catalog.video[track_name]
        assert video.codec.startswith("avc1.") or video.codec.startswith("avc3.")
        assert video.coded is not None
        assert video.coded.width == 1280
        assert video.coded.height == 720

        media_consumer = await moq.media.TrackConsumer.subscribe(broadcast_consumer, track_name, video)

        keyframe = bytes([0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC])
        media.write_frame(keyframe, timedelta(microseconds=0))

        async for frame in media_consumer:
            assert isinstance(frame, moq.media.MediaFrame)
            assert frame.timestamp // timedelta(microseconds=1) == 0
            assert len(frame.payload) > 0
            break

        break


async def test_video_publish_named_track():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "video-named-test")
    media = moq.media.TrackProducer.video(
        broadcast,
        moq.media.VideoInit(format=moq.media.VideoFormat.AVC3, data=h264_init()),
        target=moq.media.Named("hd"),
    )
    assert media.demand().name == "hd"

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        catalog = await moq.media.catalog(broadcast_consumer)
        assert list(catalog.video.keys()) == ["hd"]
        break


async def test_multiple_frames_ordering():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "ordering-test")
    media = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        catalog = await moq.media.catalog(broadcast_consumer)
        track_name = list(catalog.audio.keys())[0]
        audio = catalog.audio[track_name]
        media_consumer = await moq.media.TrackConsumer.subscribe(broadcast_consumer, track_name, audio)

        timestamps = [0, 20_000, 40_000, 60_000, 80_000]
        for i, ts in enumerate(timestamps):
            media.write_frame(f"frame-{i}".encode(), timedelta(microseconds=ts))

        for i, expected_ts in enumerate(timestamps):
            async for frame in media_consumer:
                assert isinstance(frame, moq.media.MediaFrame)
                assert frame.timestamp // timedelta(microseconds=1) == expected_ts
                assert frame.payload == f"frame-{i}".encode()
                break

        break


async def test_catalog_update_on_new_track():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "catalog-update")
    _media1 = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        cat_consumer = await moq.media.CatalogConsumer.subscribe(broadcast_consumer)

        # First catalog: 1 audio track.
        catalog1 = await anext(cat_consumer)
        assert len(catalog1.audio) == 1

        # Add a second audio track, which triggers a catalog update.
        _media2 = moq.media.TrackProducer.audio(
            broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
        )

        catalog2 = await anext(cat_consumer)
        assert len(catalog2.audio) == 2

        break


def test_close_twice_is_a_noop():
    broadcast = moq.BroadcastProducer()
    _media = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )
    broadcast.close()
    broadcast.close()

    with pytest.raises(Exception):
        moq.media.TrackProducer.audio(
            broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
        )


async def test_announced_broadcast():
    origin = moq.OriginProducer()
    _broadcast = create_announced(origin, "test/broadcast")

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        assert announcement.prefix == "test/broadcast"
        broadcast_consumer = await consumer.request(announcement.prefix)
        _catalog = await moq.media.CatalogConsumer.subscribe(broadcast_consumer)
        break


async def test_announced_epoch_restarts():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    announced = consumer.announced()
    publication = origin.create_broadcast("epoch")

    first = moq.mint_epoch()
    publication.announce(moq.AnnounceRoute(epoch=first))
    start = await next_route(announced)
    assert isinstance(start, moq.AnnounceEventStart)
    assert start.announce.route.epoch == first

    second = moq.mint_epoch()
    publication.announce(moq.AnnounceRoute(epoch=second))
    restart = await next_route(announced)
    assert isinstance(restart, moq.AnnounceEventRestart)
    assert restart.announce.route.epoch == second

    with pytest.raises(moq.Error.InvalidEpoch):
        publication.announce(moq.AnnounceRoute(epoch=first.upper()))
    with pytest.raises(moq.Error.InvalidEpoch):
        publication.announce(moq.AnnounceRoute(epoch="not-an-epoch"))
    publication.broadcast.close()


async def test_finish_serves_until_closed():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    announced = consumer.announced()
    broadcast = create_announced(origin, "finish")
    assert isinstance(await next_route(announced), moq.AnnounceEventStart)

    track = broadcast.publish_track("status")
    track.write_frame(b"tail")
    track.finish()
    moq.media.CatalogProducer(broadcast).finish()

    # A late subscriber still drains the finished track while the broadcast is up.
    late = await consumer.request("finish")
    reader = await late.subscribe_track("status")
    frame = await asyncio.wait_for(reader.read_frame(), timeout=5)
    assert frame is not None and frame.payload == b"tail"

    broadcast.close()
    assert isinstance(await next_route(announced), moq.AnnounceEventEnd)


def test_publish_lifecycle():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status")
    track.write_frame(b'{"cmd": "ready"}')
    track.finish()
    broadcast.close()


async def test_publish_track_info_and_subscription():
    """Raw track published with explicit TrackInfo, consumed with a Subscription."""
    broadcast = moq.BroadcastProducer()
    info = moq.TrackInfo(priority=5, max_age=timedelta(seconds=2))
    track = broadcast.publish_track("status", info)

    consumer = track.consume(moq.Subscription(priority=3))
    track.write_frame(b"ready")

    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == b"ready"
    track.finish()


def test_track_info_keeps_native_default_priority():
    """A partially configured TrackInfo keeps moq-ffi's default priority of 0."""
    native = moq_ffi.MoqTrackInfo().priority
    assert moq.TrackInfo()._ffi().priority == native
    assert moq.TrackInfo(max_age=timedelta(seconds=2))._ffi().priority == native
    assert moq.TrackInfo(timescale=90_000)._ffi().priority == native


def test_string_sequences_refuse_a_bare_string():
    """A bare str would split into characters, so the config refuses it up front."""
    with pytest.raises(TypeError, match="roots"):
        moq.Client("https://localhost:4443", tls=moq.ClientTls(roots="ca.pem"))
    with pytest.raises(TypeError, match="versions"):
        moq.Server(versions="moq-lite-03")


async def test_fetch_group_and_serve_dynamic_miss():
    """Fetch a cached group, then serve an uncached sequence through TrackRequests."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("events", moq.TrackInfo(timescale=1_000_000))
    consumer = broadcast.consume()

    cached = track.append_group()
    cached.write_frame(b"cached", timedelta(0))
    cached.finish()

    fetched = await consumer.fetch_group("events", 0, moq.FetchGroupOptions(priority=3))
    assert fetched.sequence == 0
    assert [frame.payload async for frame in fetched] == [b"cached"]

    dynamic = track.requests()
    pending = asyncio.create_task(consumer.fetch_group("events", 7, moq.FetchGroupOptions(priority=11)))
    fill = await asyncio.wait_for(dynamic.requested_fill(), timeout=5.0)
    assert (fill.start_group, fill.start_frame, fill.end_group) == (7, 0, 8)
    assert fill.priority == 11
    assert fill.next() == 7
    assert fill.is_used()
    assert fill.is_wanted(7)
    assert not fill.is_wanted(8)

    produced = fill.accept(7)
    produced.write_frame(b"archive", timedelta(microseconds=140_000))
    produced.finish()

    fetched = await asyncio.wait_for(pending, timeout=5.0)
    assert [frame.payload async for frame in fetched] == [b"archive"]


async def test_json_snapshot_roundtrip():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status", moq.TrackInfo(timescale=1_000_000, compress=True))
    producer = moq.json.SnapshotProducer(broadcast, track)
    consumer = moq.json.SnapshotConsumer(await broadcast.consume().subscribe_track("status"))

    for viewers in range(1, 5):
        producer.update({"viewers": viewers}, timestamp=timedelta(milliseconds=viewers))

    # Iteration yields every state in order; latest() skips the buffered third.
    value = await asyncio.wait_for(anext(consumer), timeout=5.0)
    assert value == moq.json.Timed({"viewers": 1}, timedelta(milliseconds=1))
    value = await asyncio.wait_for(anext(consumer), timeout=5.0)
    assert value == moq.json.Timed({"viewers": 2}, timedelta(milliseconds=2))
    value = await asyncio.wait_for(consumer.latest(), timeout=5.0)
    assert value == moq.json.Timed({"viewers": 4}, timedelta(milliseconds=4))

    producer.finish()
    assert await asyncio.wait_for(consumer.latest(), timeout=5.0) is None


async def test_json_stream_roundtrip():
    broadcast = moq.BroadcastProducer()
    producer = moq.json.StreamProducer(broadcast, broadcast.publish_track("events"))
    consumer = moq.json.StreamConsumer(await broadcast.consume().subscribe_track("events"))

    for n in range(3):
        producer.append({"n": n})
        record = await asyncio.wait_for(anext(consumer), timeout=5.0)
        assert record == moq.json.Timed({"n": n}, None)

    producer.finish()


async def test_json_window_roundtrip():
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("log", moq.TrackInfo(timescale=1_000_000, compress=True))
    producer = moq.json.WindowProducer(track)
    consumer = moq.json.WindowConsumer(await broadcast.consume().subscribe_track("log"))
    demand = producer.demand()
    await asyncio.wait_for(until_wanted(demand), timeout=5.0)

    for n in range(3):
        producer.push({"n": n}, timestamp=timedelta(milliseconds=n))
    producer.pop(2, timestamp=timedelta(milliseconds=3))

    for n in range(3):
        event = await asyncio.wait_for(anext(consumer), timeout=5.0)
        assert event == moq.json.Timed(moq.json.WindowPush(n, {"n": n}), timedelta(milliseconds=n))
    event = await asyncio.wait_for(anext(consumer), timeout=5.0)
    assert event == moq.json.Timed(moq.json.WindowPop(range(0, 2)), timedelta(milliseconds=3))

    producer.finish()
    with pytest.raises(moq.Error.Closed):
        producer.push({"n": 3}, timestamp=timedelta(milliseconds=4))
    assert [event async for event in consumer] == []


async def test_json_window_cut_restates_the_window():
    broadcast = moq.BroadcastProducer()
    producer = moq.json.WindowProducer(broadcast.publish_track("log"))
    raw = await broadcast.consume().subscribe_track("log")
    consumer = moq.json.WindowConsumer(await broadcast.consume().subscribe_track("log"))

    for n in range(3):
        producer.push({"n": n})
    producer.pop(1)
    events = [(await asyncio.wait_for(anext(consumer), timeout=5.0)).value for _ in range(4)]
    assert events == [
        moq.json.WindowPush(0, {"n": 0}),
        moq.json.WindowPush(1, {"n": 1}),
        moq.json.WindowPush(2, {"n": 2}),
        moq.json.WindowPop(range(0, 1)),
    ]

    producer.cut()
    producer.push({"n": 3})
    while True:
        group = await asyncio.wait_for(raw.recv_group(), timeout=5.0)
        assert group is not None
        if group.sequence > 0:
            break
    header = await asyncio.wait_for(group.read_frame(), timeout=5.0)
    assert header is not None
    assert json.loads(header.payload) == {"offset": 1, "records": [{"n": 1}, {"n": 2}, {"n": 3}]}

    # The restated records were already delivered, so the reader sees only the new one.
    event = await asyncio.wait_for(anext(consumer), timeout=5.0)
    assert event.value == moq.json.WindowPush(3, {"n": 3})
    producer.finish()


async def test_json_window_skips_records_outside_the_checkpoint():
    broadcast = moq.BroadcastProducer()
    producer = moq.json.WindowProducer(broadcast.publish_track("log", moq.TrackInfo(refresh=1)), checkpoint_records=2)
    for n in range(5):
        producer.push({"n": n})

    # The join crosses the refresh ratio, so the producer restates the bounded checkpoint.
    joiner = await broadcast.consume().subscribe_track("log")
    while True:
        group = await asyncio.wait_for(joiner.recv_group(), timeout=5.0)
        assert group is not None
        if group.sequence > 0:
            break

    consumer = moq.json.WindowConsumer(await broadcast.consume().subscribe_track("log"))
    events = [(await asyncio.wait_for(anext(consumer), timeout=5.0)).value for _ in range(3)]
    assert events == [
        moq.json.WindowSkip(range(0, 3)),
        moq.json.WindowPush(3, {"n": 3}),
        moq.json.WindowPush(4, {"n": 4}),
    ]

    producer.finish()
    with pytest.raises(moq.Error.JsonTrack):
        moq.json.WindowProducer(broadcast.publish_track("bad"), checkpoint_records=0)


async def test_json_producers_report_demand():
    broadcast = moq.BroadcastProducer()
    snapshot = moq.json.SnapshotProducer(broadcast, broadcast.publish_track("status", moq.TrackInfo(compress=True)))
    stream = moq.json.StreamProducer(broadcast, broadcast.publish_track("events"))
    snapshot_demand = snapshot.demand()
    stream_demand = stream.demand()
    assert snapshot_demand.name == "status"
    assert snapshot_demand.wanted() is None

    consumer = broadcast.consume()
    snapshot_consumer = moq.json.SnapshotConsumer(await consumer.subscribe_track("status"))
    stream_consumer = moq.json.StreamConsumer(await consumer.subscribe_track("events"))
    await asyncio.wait_for(until_wanted(snapshot_demand), timeout=5.0)
    await asyncio.wait_for(until_wanted(stream_demand), timeout=5.0)
    assert snapshot_demand.wanted() == 0

    snapshot_consumer.cancel()
    stream_consumer.cancel()
    await asyncio.wait_for(until_wanted(snapshot_demand, False), timeout=5.0)
    await asyncio.wait_for(until_wanted(stream_demand, False), timeout=5.0)

    snapshot.finish()
    with pytest.raises(moq.Error.Closed):
        await asyncio.wait_for(until_wanted(snapshot_demand), timeout=5.0)


async def test_dynamic_track_request():
    broadcast = moq.BroadcastProducer()
    dynamic = broadcast.requests()
    consumer = broadcast.consume()

    # The subscribe stays pending until the request is accepted below; run it concurrently.
    subscribe = asyncio.create_task(consumer.subscribe_track("events"))

    request = await asyncio.wait_for(dynamic.requested_track(), timeout=5.0)
    assert request.name == "events"
    demand = request.demand()
    assert demand.wanted() is not None

    # Accept the request as a raw track (which unblocks the subscribe), then write.
    track = request.accept()
    payload = b"hello dynamic track"
    track.write_frame(payload)

    track_consumer = await asyncio.wait_for(subscribe, timeout=5.0)
    frame = await asyncio.wait_for(track_consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == payload

    track_consumer.cancel()
    await asyncio.wait_for(until_wanted(demand, False), timeout=5.0)
    track.finish()
    broadcast.close()
    del track
    with pytest.raises(moq.Error.Closed):
        await asyncio.wait_for(until_wanted(demand), timeout=5.0)


async def test_media_subscribe_refuses_a_non_rendition():
    """Passing the old container argument fails loud instead of reading it as audio."""
    consumer = moq.BroadcastProducer().consume()
    with pytest.raises(TypeError, match="Video or Audio"):
        await moq.media.TrackConsumer.subscribe(consumer, "video", cast(Any, _LEGACY))


async def test_dynamic_track_request_can_publish_media():
    broadcast = moq.BroadcastProducer()
    dynamic = broadcast.requests()
    consumer = broadcast.consume()
    catalog_consumer = await moq.media.CatalogConsumer.subscribe(consumer)

    # TrackProducer.audio with Requested accepts the request (at the media timescale), which is what
    # unblocks TrackConsumer.subscribe, so run the subscribe concurrently until then.
    subscribe = asyncio.create_task(
        moq.media.TrackConsumer.subscribe(
            consumer,
            "requested-audio",
            moq.media.Audio(
                codec="opus",
                description=None,
                sample_rate=48_000,
                channel_count=2,
                bitrate=None,
                container=_LEGACY,
            ),
        )
    )

    track = await asyncio.wait_for(dynamic.requested_track(), timeout=5.0)
    assert track.name == "requested-audio"

    media = moq.media.TrackProducer.audio(
        broadcast,
        moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head()),
        target=moq.media.Requested(track),
    )
    assert media.demand().name == "requested-audio"
    with pytest.raises(Exception):
        _ = track.name

    media_consumer = await asyncio.wait_for(subscribe, timeout=5.0)

    catalog = await asyncio.wait_for(anext(catalog_consumer), timeout=5.0)
    audio = catalog.audio["requested-audio"]
    assert audio.codec == "opus"
    assert audio.sample_rate == 48000
    assert audio.channel_count == 2

    payload = b"dynamic opus frame"
    media.write_frame(payload, timedelta(microseconds=20_000))

    async for frame in media_consumer:
        assert isinstance(frame, moq.media.MediaFrame)
        assert frame.payload == payload
        assert frame.timestamp // timedelta(microseconds=1) == 20_000
        break

    media.finish()


async def test_dynamic_broadcast_request():
    origin = moq.OriginProducer(cache_capacity_bytes=4096)
    dynamic = origin.requests("")
    consumer = origin.consume()

    requesting = asyncio.create_task(consumer.request("dynamic/broadcast"))

    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "dynamic/broadcast"

    prepared = request.prepare(moq.mint_epoch())
    track = prepared.broadcast.publish_track("status", moq.TrackInfo(timescale=1_000_000))
    served = prepared.publish()
    assert served.broadcast is prepared.broadcast
    with pytest.raises(Exception):
        _ = request.path

    broadcast = await asyncio.wait_for(requesting, timeout=5.0)
    track_consumer = await broadcast.subscribe_track("status")
    payload = b"served dynamically"
    track.write_frame(payload, timedelta(microseconds=20_000))

    frame = await asyncio.wait_for(track_consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == payload
    assert frame.timestamp == timedelta(microseconds=20_000)
    track.finish()
    served.broadcast.close()


async def test_dynamic_broadcast_request_can_reject():
    origin = moq.OriginProducer()
    dynamic = origin.requests("")
    consumer = origin.consume()

    requesting = asyncio.create_task(consumer.request("missing"))
    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "missing"

    request.reject(404)
    with pytest.raises(Exception):
        _ = request.path

    with pytest.raises(Exception):
        await asyncio.wait_for(requesting, timeout=5.0)


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
    track = broadcast.publish_track("chunks", moq.TrackInfo(timescale=1_000_000))

    group = track.append_group()
    for i in range(10):
        group.write_frame(f"frame-{i}".encode(), timedelta(microseconds=i))
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
        track.write_frame(b"late")


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
    track = broadcast.publish_track("parallel", moq.TrackInfo(timescale=1_000_000))

    g0 = track.append_group()
    g1 = track.append_group()
    assert g0.sequence == 0
    assert g1.sequence == 1

    g0.write_frame(b"a0", timedelta(0))
    g1.write_frame(b"b0", timedelta(0))
    g0.write_frame(b"a1", timedelta(microseconds=1))
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
        tls=moq.ClientTls(roots=["root.pem"], fingerprints=["abc123"]),
    )
    assert client._config.tls.roots == ["root.pem"]
    assert client._config.tls.fingerprints == ["abc123"]


def test_client_tokens_reach_the_config():
    """Tokens reach the config every session starts from."""
    client = moq.connect("https://example.com", tokens=["first", "second"])
    assert client._config.tokens == ["first", "second"]


async def test_container_consumer_default_latency_and_context_manager():
    """TrackConsumer.subscribe defaults the
    latency; the returned consumer is also an async context manager."""
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "live")
    media = moq.media.TrackProducer.audio(
        broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head())
    )

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        catalog = await moq.media.catalog(broadcast_consumer)
        track_name, audio = next(iter(catalog.audio.items()))

        # No container argument, no explicit latency.
        payload = b"opus audio payload data"
        media.write_frame(payload, timedelta(microseconds=1_000_000))

        async with await moq.media.TrackConsumer.subscribe(broadcast_consumer, track_name, audio) as media_consumer:
            async for frame in media_consumer:
                assert isinstance(frame, moq.media.MediaFrame)
                assert frame.payload == payload
                break

        break


async def test_raw_publish_consume():
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "robot/arm")
    raw = broadcast.publish_track("events")

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        assert announcement.prefix == "robot/arm"

        broadcast_consumer = await consumer.request(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track("events")

        payload = b'{"cmd": "button_changed", "arm": "left", "button": "THUMB", "state": "PRESSED"}'
        raw.write_frame(payload)

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

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        subscription = moq.Subscription(max_delay=timedelta(seconds=1), ranges=FROM_GROUP_0)
        raw_consumer = await broadcast_consumer.subscribe_track("commands", subscription)

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
                received.append(frame.payload)
            if len(received) == len(messages):
                break

        assert received == messages
        break


async def test_raw_producer_consume_direct():
    """Consume a raw track directly from the producer, no origin/broadcast plumbing."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("direct")
    consumer = track.consume(moq.Subscription(max_delay=timedelta(seconds=1), ranges=FROM_GROUP_0))

    track.write_frame(b"hello")
    track.write_frame(b"world")

    received = []
    async for group in consumer:
        async for frame in group:
            received.append(frame.payload)
        if len(received) == 2:
            break

    assert received == [b"hello", b"world"]


async def test_ascending_subscription_reads_a_cached_backlog_oldest_first():
    """An ascending subscription reaching back over a cached backlog reads it oldest first."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("backlog", moq.TrackInfo(timescale=1_000_000))
    for ms in range(3):
        group = track.append_group()
        group.write_frame(b"x", timedelta(milliseconds=ms))
        group.finish()

    consumer = track.consume(
        moq.Subscription(
            max_delay=timedelta(seconds=10),
            ranges=[moq.Range(start=cast(moq.Anchor, moq.Anchor.RELATIVE(groups=2)))],
            order=moq.Order.ASCENDING,
        )
    )
    sequences = []
    for _ in range(3):
        group = await asyncio.wait_for(consumer.recv_group(), timeout=5.0)
        assert group is not None
        sequences.append(group.sequence)
    assert sequences == [0, 1, 2]


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

    received = [frame.payload async for frame in group_consumer]
    assert received == [b"a", b"b"]


async def test_broadcast_producer_consume_direct():
    """Consume a broadcast directly from the producer with catalog and raw track."""
    broadcast = moq.BroadcastProducer()
    raw = broadcast.publish_track("events")
    consumer = broadcast.consume()

    raw_consumer = await consumer.subscribe_track("events")
    raw.write_frame(b"event-0")

    async for group in raw_consumer:
        async for frame in group:
            assert frame.payload == b"event-0"
            break
        break


async def test_raw_group_sequence():
    """Consumer sees the same sequence numbers the producer assigned."""
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "track/seq")
    raw = broadcast.publish_track("seq", moq.TrackInfo(timescale=1_000_000))

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track(
            "seq", moq.Subscription(max_delay=timedelta(seconds=1), ranges=FROM_GROUP_0)
        )

        sent_sequences = []
        for i in range(3):
            group = raw.append_group()
            sent_sequences.append(group.sequence)
            group.write_frame(f"msg-{i}".encode(), timedelta(microseconds=i))
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

    subscription = moq.Subscription(max_delay=timedelta(seconds=1), ranges=FROM_GROUP_0)
    seq_consumer = raw.consume(subscription)
    arr_consumer = raw.consume(subscription)

    for sequence in (5, 3):
        group = raw.create_group(sequence)
        group.write_frame(f"group-{sequence}".encode())
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


def _legacy(timestamp_us: int, payload: bytes) -> bytes:
    """Encode a hang legacy frame: a 4-byte QUIC varint µs timestamp, then the payload."""
    assert timestamp_us < 1 << 30
    return (timestamp_us | 0x8000_0000).to_bytes(4, "big") + payload


_LEGACY = cast(moq.media.Container, moq.media.Container.LEGACY())


async def _recv_raw(track: moq.TrackConsumer) -> moq.GroupConsumer:
    group = await asyncio.wait_for(track.recv_group(), timeout=5.0)
    assert group is not None
    return group


async def _recv_media(track: moq.TrackConsumer) -> moq.media.GroupConsumer:
    return moq.media.GroupConsumer(await _recv_raw(track), _LEGACY)


async def test_container_group_decodes_arrival_order():
    """Raw groups received in arrival order decode as their frames land: a
    stalled group is neither skipped nor blocks a newer one, and each timestamp
    is the container's media time, not the transport's."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("audio", moq.TrackInfo(timescale=1_000_000))
    consumer = await broadcast.consume().subscribe_track("audio")

    first = track.append_group()
    first.write_frame(_legacy(1_000_000, b"a"), timedelta(milliseconds=1))
    first_media = await _recv_media(consumer)
    frame = await asyncio.wait_for(anext(first_media), timeout=5.0)
    assert (frame.payload, frame.timestamp, frame.keyframe) == (b"a", timedelta(seconds=1), True)

    # A newer group lands while the first is stalled mid-way.
    second = track.append_group()
    second.write_frame(_legacy(1_040_000, b"c"), timedelta(milliseconds=2))
    second_media = await _recv_media(consumer)
    assert second_media.sequence == 1
    frame = await asyncio.wait_for(anext(second_media), timeout=5.0)
    assert (frame.payload, frame.timestamp) == (b"c", timedelta(microseconds=1_040_000))

    # The stalled group resumes and is still delivered.
    first.write_frame(_legacy(1_020_000, b"b"), timedelta(milliseconds=3))
    frame = await asyncio.wait_for(anext(first_media), timeout=5.0)
    assert (frame.payload, frame.timestamp, frame.keyframe) == (b"b", timedelta(microseconds=1_020_000), False)

    first.finish()
    second.finish()
    assert [frame async for frame in first_media] == []
    assert [frame async for frame in second_media] == []


async def test_container_group_takes_over_an_unread_group():
    """Decoding refuses a raw group that already read a frame, and closes the one it takes."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("audio", moq.TrackInfo(timescale=1_000_000))
    consumer = await broadcast.consume().subscribe_track("audio")

    group = track.append_group()
    group.write_frame(_legacy(1_000_000, b"a"), timedelta(milliseconds=1))
    raw = await _recv_raw(consumer)
    assert await raw.read_frame() is not None
    with pytest.raises(moq.Error.AlreadyCommitted):
        moq.media.GroupConsumer(raw, _LEGACY)

    group = track.append_group()
    group.write_frame(_legacy(1_020_000, b"b"), timedelta(milliseconds=2))
    raw = await _recv_raw(consumer)
    media = moq.media.GroupConsumer(raw, _LEGACY)
    with pytest.raises(moq.Error.Cancelled):
        await raw.read_frame()
    frame = await asyncio.wait_for(anext(media), timeout=5.0)
    assert frame.timestamp == timedelta(microseconds=1_020_000)


async def test_container_group_fails_loudly():
    """An unconfigurable container fails before taking the group; an undecodable frame fails the read."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("audio", moq.TrackInfo(timescale=1_000_000))
    consumer = await broadcast.consume().subscribe_track("audio")

    group = track.append_group()
    # A truncated varint timestamp: the 2-byte form with only its first byte.
    group.write_frame(b"\x40", timedelta(milliseconds=1))
    raw = await _recv_raw(consumer)
    with pytest.raises(moq.Error.Codec):
        moq.media.GroupConsumer(raw, cast(moq.media.Container, moq.media.Container.CMAF(init=b"")))
    media = moq.media.GroupConsumer(raw, _LEGACY)
    # UniFFI rebinds the error class, hiding its Exception base from pyright.
    with pytest.raises(moq.Error):  # type: ignore[attr-defined]
        await asyncio.wait_for(anext(media), timeout=5.0)


async def test_raw_multi_frame_group():
    """A single group can carry multiple frames, not just one per group."""
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "stream/chunks")
    raw = broadcast.publish_track("chunks")

    consumer = origin.consume()

    async for announcement in routes(consumer.announced()):
        broadcast_consumer = await consumer.request(announcement.prefix)
        raw_consumer = await broadcast_consumer.subscribe_track("chunks")

        group_producer = raw.append_group()
        chunks = [b"chunk-0", b"chunk-1", b"chunk-2"]
        for chunk in chunks:
            group_producer.write_frame(chunk)
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
    consumer = track.consume(moq.Subscription(max_delay=timedelta(seconds=1), ranges=FROM_GROUP_0))

    track.write_frame(b"ready")
    track.write_frame(b"running")
    track.write_frame(b"done")

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
    track = broadcast.publish_track("status", moq.TrackInfo(timescale=1_000_000))
    consumer = track.consume()

    track.write_frame(b"ready", timedelta(microseconds=12_345))
    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"ready"
    assert frame.timestamp == timedelta(microseconds=12_345)

    group = track.append_group()
    group_consumer = group.consume()
    group.write_frame(b"group", timedelta(microseconds=23_456))
    group.finish()

    frame = await group_consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"group"
    assert frame.timestamp == timedelta(microseconds=23_456)


async def test_read_frame_skips_remaining_frames_in_group():
    """read_frame() only returns the first frame of a multi-frame group."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("mixed")
    consumer = track.consume(moq.Subscription(max_delay=timedelta(seconds=1), ranges=FROM_GROUP_0))

    group = track.append_group()
    group.write_frame(b"first")
    group.write_frame(b"second-ignored")
    group.finish()

    track.write_frame(b"next-group-first")

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

    track.write_frame(b"only")
    track.finish()

    frame = await consumer.read_frame()
    assert frame is not None
    assert frame.payload == b"only"
    assert await consumer.read_frame() is None


async def test_read_frame_skips_empty_group_on_open_track():
    """An empty completed group is not track EOF while the track is still open."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status", moq.TrackInfo(timescale=1_000_000))
    consumer = track.consume()

    track.append_group().finish()

    read = asyncio.create_task(consumer.read_frame())
    await asyncio.sleep(0.05)
    assert not read.done(), "empty group must not end an open track"

    track.write_frame(b"after-empty", timedelta(microseconds=1_000))
    frame = await asyncio.wait_for(read, timeout=5.0)
    assert frame is not None
    assert frame.payload == b"after-empty"
    assert frame.timestamp == timedelta(microseconds=1_000)


async def test_read_frame_skips_empty_then_populated_groups():
    """read_frame() walks past completed empty groups to the next first frame."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status", moq.TrackInfo(timescale=1_000_000))
    consumer = track.consume()

    track.append_group().finish()
    track.append_group().finish()
    track.write_frame(b"populated", timedelta(microseconds=2_000))

    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == b"populated"
    assert frame.timestamp == timedelta(microseconds=2_000)


async def test_read_frame_keeps_group_across_cancelled_call():
    """Cancelling one read_frame() must not drop the group it already acquired."""
    broadcast = moq.BroadcastProducer()
    track = broadcast.publish_track("status", moq.TrackInfo(timescale=1_000_000))
    consumer = track.consume()

    group = track.append_group()
    read = asyncio.create_task(consumer.read_frame())
    await asyncio.sleep(0.05)
    assert not read.done()
    read.cancel()
    with pytest.raises(asyncio.CancelledError):
        await read

    group.write_frame(b"kept", timedelta(microseconds=3_000))
    group.finish()
    track.finish()

    frame = await asyncio.wait_for(consumer.read_frame(), timeout=5.0)
    assert frame is not None
    assert frame.payload == b"kept"
    assert frame.timestamp == timedelta(microseconds=3_000)
    assert await asyncio.wait_for(consumer.read_frame(), timeout=5.0) is None


def test_audio_encoder_returns_namespaced_records():
    """The codec-only encoder's results are named under ``moq.audio``."""
    encoder = moq.audio.Encoder(moq.audio.EncoderOutput(codec=moq.AudioCodec.opus(), sample_rate=48000, channels=1))
    packet = encoder.encode([0.0] * encoder.frame_size)
    assert isinstance(packet, moq.audio.Packet)
    assert packet.data
    finish = encoder.finish()
    assert isinstance(finish, moq.audio.EncoderFinish)


def test_audio_encoder_frame_init_and_bitrate():
    """A frame duration sets the frame size, the encoder reports its OpusHead, and the bitrate retunes live."""
    output = moq.audio.EncoderOutput(
        codec=moq.AudioCodec.opus(), sample_rate=48000, channels=1, frame_duration=timedelta(milliseconds=10)
    )
    encoder = moq.audio.Encoder(output)
    assert encoder.frame_size == 480
    init = encoder.track_init()
    assert init.format == moq.media.AudioFormat.OPUS
    assert init.data[:8] == b"OpusHead"
    encoder.set_bitrate(32_000)
    assert encoder.encode([0.0] * encoder.frame_size).data

    with pytest.raises(moq.Error.Audio):
        moq.audio.Encoder(replace(output, frame_duration=timedelta(milliseconds=15)))


def test_video_namespace_names_its_configuration_types():
    """``moq.video`` names every type its records take."""
    output = moq.video.EncoderOutput(
        codec=moq.video.Codec.H264,
        kind=cast(moq.video.EncoderKind, moq.video.EncoderKind.AUTO()),
    )
    assert output.codec == moq.VideoCodec.H264
    assert moq.video.PixelFormat.RGBA == moq.VideoPixelFormat.RGBA


def test_encode_audio_with_opus_object():
    """Construct the Opus selection, encode actual audio, release in either order."""
    for codec_first in (True, False):
        broadcast = moq.BroadcastProducer()
        codec = moq.AudioCodec.opus()
        output = moq.audio.EncoderOutput(
            codec=codec,
        )
        producer = moq.audio.Producer(
            moq.media.CatalogProducer(broadcast),
            moq.AudioEncoderInput(format=moq.AudioSampleFormat.F32, sample_rate=48_000, channels=1),
            output,
            track="mic",
        )
        if codec_first:
            del codec
            del output
        else:
            del output
            del codec
        # One 20 ms Opus frame of silence at 48 kHz mono.
        producer.write(moq.audio.Frame(timestamp=timedelta(0), data=bytes(960 * 4)))
        assert producer.demand().name == "mic"
        producer.finish()
        broadcast.close()


async def test_audio_consumer_reports_its_playout_delay():
    """A fixed playout delay is reported back as is; an estimated one moves as frames are read."""
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "audio-delay")
    audio = moq.audio.Producer(
        moq.media.CatalogProducer(broadcast),
        moq.AudioEncoderInput(format=moq.AudioSampleFormat.F32, sample_rate=48_000, channels=1),
        moq.audio.EncoderOutput(
            codec=moq.AudioCodec.opus(),
        ),
        track="mic",
    )

    consumer = origin.consume()
    broadcast_consumer = await asyncio.wait_for(consumer.request("audio-delay"), timeout=5.0)
    catalog = await asyncio.wait_for(moq.media.catalog(broadcast_consumer), timeout=5.0)
    rendition = catalog.audio["mic"]

    def output(delay: timedelta | None) -> moq.audio.DecoderOutput:
        return moq.audio.DecoderOutput(format=moq.AudioSampleFormat.F32, delay=delay)

    fixed = await moq.audio.Consumer.subscribe(broadcast_consumer, "mic", rendition, output(timedelta(milliseconds=80)))
    estimated = await moq.audio.Consumer.subscribe(broadcast_consumer, "mic", rendition, output(None))
    assert fixed.delay == timedelta(milliseconds=80)
    initial = estimated.delay

    # Two 20 ms Opus frames of silence at 48 kHz mono.
    for i in range(2):
        audio.write(moq.audio.Frame(timestamp=timedelta(milliseconds=20 * i), data=bytes(960 * 4)))
    assert isinstance(await asyncio.wait_for(anext(fixed), timeout=5.0), moq.audio.Frame)
    assert isinstance(await asyncio.wait_for(anext(estimated), timeout=5.0), moq.audio.Frame)

    assert fixed.delay == timedelta(milliseconds=80)
    assert estimated.delay != initial

    fixed.cancel()
    estimated.cancel()
    audio.finish()
    broadcast.close()


async def test_decode_video_frame():
    """A decoded frame owns its picture and converts to either layout, even after its consumer is cancelled."""
    origin = moq.OriginProducer()
    broadcast = create_announced(origin, "video-decode-frame")
    video = moq.video.Producer(
        moq.media.CatalogProducer(broadcast),
        moq.VideoEncoderInput(
            format=moq.VideoPixelFormat.RGBA,
            width=320,
            height=240,
            framerate=moq.video.FrameRate(numerator=30000, denominator=1001),
        ),
        # Software so the encode is deterministic everywhere. uniffi nests each
        # variant inside the enum without declaring it a subclass, so the cast is
        # what makes the variant typecheck.
        moq.VideoEncoderOutput(
            codec=moq.VideoCodec.H264,
            kind=cast(moq.VideoEncoderKind, moq.VideoEncoderKind.SOFTWARE()),
        ),
        track="camera",
    )

    # Seed the track so a subscriber joining below lands on encoded media.
    rgba = bytes([0x80]) * (320 * 240 * 4)
    video.cut()
    for i in range(10):
        video.write(moq.video.Frame(timestamp=timedelta(microseconds=i * 33_333), data=rgba))

    consumer = origin.consume()
    broadcast_consumer = await asyncio.wait_for(consumer.request("video-decode-frame"), timeout=5.0)
    catalog = await asyncio.wait_for(moq.media.catalog(broadcast_consumer), timeout=5.0)
    track_name = next(iter(catalog.video))
    rendition = catalog.video[track_name]

    decoder = await moq.video.Consumer.subscribe(broadcast_consumer, track_name, rendition)

    # Keep the encoder fed so the decoder sees frames after it joined.
    for i in range(10, 40):
        video.write(moq.video.Frame(timestamp=timedelta(microseconds=i * 33_333), data=rgba))

    frame = await asyncio.wait_for(anext(decoder), timeout=5.0)
    decoder.cancel()
    assert isinstance(frame, moq.VideoDecodedFrame)

    i420 = frame.pixels(moq.VideoPixelFormat.I420)

    assert len(i420) == frame.width() * frame.height() * 3 // 2

    packed = frame.pixels(moq.VideoPixelFormat.RGBA)
    assert len(packed) == frame.width() * frame.height() * 4
    assert all(packed[i] == 0xFF for i in range(3, len(packed), 4)), "RGBA output should be opaque"

    video.finish()
    broadcast.close()


async def test_broadcast_is_reachable_only_while_announced():
    origin = moq.OriginProducer()
    publication = origin.create_broadcast("live")
    track = publication.broadcast.publish_track("events")
    consumer = origin.consume()
    with pytest.raises(Exception):
        await asyncio.wait_for(consumer.request("live"), timeout=5.0)

    publication.announce()
    announced = consumer.announced()
    first = await next_route(announced)
    assert isinstance(first, moq.AnnounceEventStart)
    assert first.announce.prefix == "live"

    publication.unannounce()
    retracted = await next_route(announced)
    assert isinstance(retracted, moq.AnnounceEventEnd)
    assert retracted.announce.prefix == "live"
    with pytest.raises(Exception):
        await asyncio.wait_for(consumer.request("live"), timeout=5.0)

    publication.announce()
    back = await next_route(announced)
    assert isinstance(back, moq.AnnounceEventStart)
    await asyncio.wait_for(consumer.request("live"), timeout=5.0)
    announced.cancel()
    track.finish()
    publication.broadcast.close()


async def test_an_epoch_pins_request_and_follow():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    epoch = moq.mint_epoch()
    publication = origin.create_broadcast("live")
    publication.announce(moq.AnnounceRoute(epoch=epoch))

    await asyncio.wait_for(consumer.request("live", epoch=epoch), timeout=5.0)
    with pytest.raises(moq.Error.Protocol):
        await asyncio.wait_for(consumer.request("live", epoch=moq.mint_epoch()), timeout=5.0)
    with pytest.raises(moq.Error.InvalidEpoch):
        consumer.follow("live", epoch="not-an-epoch")
    async with consumer.follow("live", epoch=epoch) as following:
        await asyncio.wait_for(following.available(), timeout=5.0)
    publication.broadcast.close()


async def test_dropping_a_publication_keeps_its_route():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    publication = origin.create_broadcast("live")
    broadcast = publication.broadcast
    assert publication.broadcast is broadcast
    publication.announce()
    del publication
    await asyncio.wait_for(consumer.request("live"), timeout=5.0)

    broadcast.close()
    with pytest.raises(moq.Error.Protocol):
        await asyncio.wait_for(consumer.request("live"), timeout=5.0)


async def test_a_stale_publication_leaves_its_replacement_alone():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    stale = origin.create_broadcast("live")
    stale.announce()
    stale.broadcast.close()

    replacement = origin.create_broadcast("live")
    replacement.announce()
    with pytest.raises(moq.Error.Closed):
        stale.announce()
    with pytest.raises(moq.Error.Closed):
        stale.unannounce()
    await asyncio.wait_for(consumer.request("live"), timeout=5.0)
    replacement.broadcast.close()


async def test_announced_pattern_captures():
    origin = moq.OriginProducer()
    consumer = origin.consume()
    announced = consumer.announced("room", filter="*/chat")

    dynamic = origin.requests("room")
    overlap = await next_route(announced)
    assert isinstance(overlap, moq.AnnounceEventStart)
    assert overlap.announce.prefix == "room"
    assert overlap.announce.captures is None

    audio = create_announced(origin, "room/alice/audio")
    chat = create_announced(origin, "room/alice/chat")
    match = await next_route(announced)
    assert isinstance(match, moq.AnnounceEventStart)
    assert match.announce.prefix == "room/alice/chat"
    assert match.announce.captures == ["alice"]

    announced.cancel()
    dynamic.cancel()
    audio.close()
    chat.close()


async def test_dynamic_serves_a_request_under_a_prefix():
    origin = moq.OriginProducer()
    dynamic = origin.requests("live")
    consumer = origin.consume()

    pending = asyncio.create_task(consumer.request("live/cam"))
    request = await asyncio.wait_for(dynamic.requested_broadcast(), timeout=5.0)
    assert request.path == "live/cam"
    served = request.prepare(moq.mint_epoch()).publish()
    await asyncio.wait_for(pending, timeout=5.0)
    dynamic.cancel()
    served.broadcast.close()


async def test_dynamic_and_json_handles_are_async_context_managers():
    """Every handle whose only cleanup is cancel() releases it on `async with` exit."""

    async def assert_cancelled(awaitable) -> None:
        with pytest.raises(Exception) as excinfo:
            await asyncio.wait_for(awaitable, timeout=5.0)
        assert moq.is_shutdown(excinfo.value)

    origin = moq.OriginProducer()
    async with origin.requests("live") as origin_dynamic:
        pass
    await assert_cancelled(origin_dynamic.requested_broadcast())

    broadcast = moq.BroadcastProducer()
    async with broadcast.requests() as broadcast_dynamic:
        pass
    await assert_cancelled(broadcast_dynamic.requested_track())

    track = broadcast.publish_track("events")
    async with track.requests() as track_dynamic:
        pass
    await assert_cancelled(track_dynamic.requested_fill())

    snapshot = moq.json.SnapshotProducer(broadcast, broadcast.publish_track("state"))
    async with moq.json.SnapshotConsumer(await broadcast.consume().subscribe_track("state")) as snapshot_consumer:
        pass
    await assert_cancelled(anext(snapshot_consumer))

    stream = moq.json.StreamProducer(broadcast, broadcast.publish_track("log"))
    async with moq.json.StreamConsumer(await broadcast.consume().subscribe_track("log")) as stream_consumer:
        pass
    await assert_cancelled(anext(stream_consumer))

    window = moq.json.WindowProducer(broadcast.publish_track("window"))
    async with moq.json.WindowConsumer(await broadcast.consume().subscribe_track("window")) as window_consumer:
        pass
    await assert_cancelled(anext(window_consumer))

    snapshot.finish()
    stream.finish()
    window.finish()
    track.finish()
    broadcast.close()


def test_media_catalog_handle_closes_with_broadcast():
    broadcast = moq.BroadcastProducer()
    catalog = moq.media.CatalogProducer(broadcast)
    catalog.set_section("app", {"value": 42})
    broadcast.close()
    with pytest.raises(moq.Error.Closed):
        catalog.remove_section("app")


def test_container_timebase_belongs_to_one_broadcast():
    owner = moq.BroadcastProducer()
    other = moq.BroadcastProducer()
    timebase = moq.media.Timebase(owner)
    with pytest.raises(moq.Error.Codec, match="another broadcast"):
        moq.media.ContainerProducer(
            other, moq.media.ContainerInit(moq.media.ContainerFormat.FMP4, b"", timebase=timebase)
        )
    owner.close()
    with pytest.raises(moq.Error.Closed):
        moq.media.Timebase(owner)
    other.close()

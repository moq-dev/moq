import asyncio
import struct
from datetime import timedelta
from types import SimpleNamespace

import moq
import pytest

from moq_carla_sim import bridge
from moq_carla_sim.bridge import (
    Bridge,
    BridgeError,
    Config,
    Error,
    Frame,
    Hello,
    Rendition,
    decode_event,
    encode_close,
    encode_demand,
    encode_hello,
    encode_keyframe,
    encode_open,
    read_event,
)

# Shared with demo/carla/bridge/src/protocol.rs: both sides must produce and accept
# exactly these bytes.
HELLO_HEX = "03000000010100"
OPEN_HEX = "2c00000002070000000c00747275636b312f66726f6e740102007364aa010000f000000000350c00000000001e000000"
DEMAND_HEX = "06000000030700000002"
KEYFRAME_HEX = "06000000040700000001"
CLOSE_HEX = "050000000507000000"
EVENT_HELLO_HEX = "03000000810100"
CONFIG_HEX = "0f000000820700000000050000000000000167"
FRAME_HEX = "20000000830700000001d204000000000000404b4c000000000001050000000000000165"
ERROR_HEX = "0b00000084000000000400626f6f6d"

SD = Rendition("sd", 426, 240, 800_000, 30)
HD = Rendition("hd", 1280, 720, 4_000_000, 30)


def test_requests_match_the_shared_vectors() -> None:
    assert encode_hello(1).hex() == HELLO_HEX
    assert encode_open(7, "truck1/front", [SD]).hex() == OPEN_HEX
    assert encode_demand(7, 0b10).hex() == DEMAND_HEX
    assert encode_keyframe(7, 1).hex() == KEYFRAME_HEX
    assert encode_close(7).hex() == CLOSE_HEX


def _decode(hex: str):
    wire = bytes.fromhex(hex)
    (length,) = struct.unpack("<I", wire[:4])
    assert length == len(wire) - 4
    return decode_event(wire[4], wire[5:])


def test_events_match_the_shared_vectors() -> None:
    assert _decode(EVENT_HELLO_HEX) == Hello(1)
    assert _decode(CONFIG_HEX) == Config(7, 0, bytes([0, 0, 0, 1, 0x67]))
    assert _decode(FRAME_HEX) == Frame(7, 1, 1234, 5_000_000, True, bytes([0, 0, 0, 1, 0x65]))
    assert _decode(ERROR_HEX) == Error(0, "boom")


@pytest.mark.parametrize(
    "hex",
    [
        "0400000083070000",  # truncated frame
        "0400000081010000",  # trailing bytes after a hello
        "0100000099",  # unknown kind
        # keyframe flag of 2
        "20000000830700000001d204000000000000404b4c000000000002050000000000000165",
    ],
)
def test_rejects_malformed_events(hex: str) -> None:
    with pytest.raises(BridgeError):
        _decode(hex)


def _reader(data: bytes) -> asyncio.StreamReader:
    reader = asyncio.StreamReader()
    reader.feed_data(data)
    reader.feed_eof()
    return reader


def test_read_event_distinguishes_a_clean_end_from_a_torn_message() -> None:
    async def scenario() -> None:
        assert await read_event(_reader(b"")) is None
        with pytest.raises(BridgeError):
            await read_event(_reader(bytes.fromhex(FRAME_HEX)[:10]))
        with pytest.raises(BridgeError):
            await read_event(_reader(bytes.fromhex("00000000")))
        assert await read_event(_reader(bytes.fromhex(ERROR_HEX))) == Error(0, "boom")

    asyncio.run(scenario())


class FakeDemand:
    """`moq.TrackDemand`: the first `wanted_changed` returns at once, later ones wait for a change."""

    def __init__(self) -> None:
        self._wanted: int | None = None
        self._fresh = True
        self._changed = asyncio.Event()

    def want(self, group: int | None) -> None:
        self._wanted = group
        self._fresh = True
        self._changed.set()

    async def wanted_changed(self) -> int | None:
        if not self._fresh:
            self._changed.clear()
            await self._changed.wait()
        self._fresh = False
        return self._wanted


def microseconds(timestamp: timedelta) -> int:
    return timestamp // timedelta(microseconds=1)


class FakeTrack:
    def __init__(self, init: bytes, name: str, hint) -> None:
        self.init = init
        self.name = name
        self.hint = hint
        self.frames: list[tuple[bytes, int]] = []
        self.flushed: list[int] = []
        self.finished = False
        self.demand_handle = FakeDemand()

    def demand(self) -> FakeDemand:
        return self.demand_handle

    def write_frame(self, payload: bytes, timestamp: timedelta) -> None:
        self.frames.append((payload, microseconds(timestamp)))

    def flush(self, timestamp: timedelta) -> None:
        self.flushed.append(microseconds(timestamp))

    def finish(self) -> None:
        self.finished = True


class FakeBroadcast:
    def __init__(self) -> None:
        self.tracks: dict[str, FakeTrack] = {}

    def video(self, init, target) -> FakeTrack:
        assert init.format == "avc3"
        self.tracks[target.name] = FakeTrack(init.data, target.name, init.hint)
        return self.tracks[target.name]


@pytest.fixture(autouse=True)
def media_tracks(monkeypatch: pytest.MonkeyPatch) -> None:
    def video(broadcast: FakeBroadcast, init, *, target) -> FakeTrack:
        return broadcast.video(init, target)

    monkeypatch.setattr(moq.media, "TrackProducer", SimpleNamespace(video=video), raising=False)


def _message(kind: int, body: bytes) -> bytes:
    return struct.pack("<IB", len(body) + 1, kind) + body


def _config(id: int, rendition: int, init: bytes) -> bytes:
    return _message(0x82, struct.pack("<IBI", id, rendition, len(init)) + init)


def _frame(id: int, rendition: int, number: int, timestamp_us: int, keyframe: bool, payload: bytes) -> bytes:
    body = struct.pack("<IBQQBI", id, rendition, number, timestamp_us, int(keyframe), len(payload)) + payload
    return _message(0x83, body)


async def _request(reader: asyncio.StreamReader) -> tuple[int, bytes]:
    (length,) = struct.unpack("<I", await reader.readexactly(4))
    message = await reader.readexactly(length)
    return message[0], message[1:]


class FakeBridge:
    """The CARLA side of the socket, scripted by each test."""

    def __init__(self) -> None:
        self.connected: asyncio.Future[tuple[asyncio.StreamReader, asyncio.StreamWriter]] = (
            asyncio.get_running_loop().create_future()
        )

    async def serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        kind, body = await _request(reader)
        assert (kind, body) == (0x01, struct.pack("<H", bridge.VERSION))
        writer.write(bytes.fromhex(EVENT_HELLO_HEX))
        self.connected.set_result((reader, writer))


async def _connected(bridge_socket) -> tuple[Bridge, asyncio.StreamReader, asyncio.StreamWriter, asyncio.Server]:
    fake = FakeBridge()
    path = bridge_socket
    server = await asyncio.start_unix_server(fake.serve, path)
    client = await Bridge.connect(path)
    reader, writer = await fake.connected
    return client, reader, writer, server


def test_stream_advertises_publishes_and_forwards_demand(bridge_socket) -> None:
    async def scenario() -> None:
        client, reader, writer, server = await _connected(bridge_socket)
        broadcast = FakeBroadcast()
        async with asyncio.TaskGroup() as group:
            run = group.create_task(client.run())
            opening = group.create_task(client.open("truck1/front", [SD, HD], broadcast))

            kind, body = await _request(reader)
            assert _message(kind, body) == encode_open(1, "truck1/front", [SD, HD])
            writer.write(_config(1, 0, b"\x00\x00\x00\x01\x67sd"))
            writer.write(_config(1, 1, b"\x00\x00\x00\x01\x67hd"))
            stream = await opening

            # Both tracks exist, with their parameter sets, before any frame.
            assert broadcast.tracks["sd"].init.endswith(b"sd")
            assert broadcast.tracks["hd"].hint.coded.width == 1280
            assert broadcast.tracks["hd"].hint.bitrate == 4_000_000

            demand = group.create_task(stream.watch_demand())
            broadcast.tracks["hd"].demand_handle.want(0)
            assert await _request(reader) == (0x03, struct.pack("<IB", 1, 0b10))
            broadcast.tracks["sd"].demand_handle.want(0)
            assert await _request(reader) == (0x03, struct.pack("<IB", 1, 0b11))

            # Timestamps are rebased onto the first frame's simulation time and
            # flushed as live encoder output.
            writer.write(_frame(1, 1, 100, 5_000_000, True, b"idr"))
            writer.write(_frame(1, 0, 100, 5_000_000, True, b"idr-sd"))
            writer.write(_frame(1, 1, 101, 5_033_333, False, b"p"))
            await writer.drain()
            while stream.published != [1, 2]:
                await asyncio.sleep(0)
            assert broadcast.tracks["hd"].frames == [(b"idr", 0), (b"p", 33_333)]
            assert broadcast.tracks["hd"].flushed == [0, 33_333]
            assert broadcast.tracks["sd"].frames == [(b"idr-sd", 0)]
            broadcast.tracks["hd"].demand_handle.want(None)
            assert await _request(reader) == (0x03, struct.pack("<IB", 1, 0b01))

            stream.keyframe(0)
            assert await _request(reader) == (0x04, struct.pack("<IB", 1, 0))

            demand.cancel()
            stream.finish()
            assert (await _request(reader))[0] == 0x05
            assert broadcast.tracks["sd"].finished and broadcast.tracks["hd"].finished
            # The cancelled watcher clears its bit only now, after the close:
            # no demand may follow it. A raw keyframe marks the next request.
            await asyncio.gather(demand, return_exceptions=True)
            client.keyframe(9, 0)
            assert await _request(reader) == (0x04, struct.pack("<IB", 9, 0))

            # A frame the bridge encoded before it saw the close is dropped, not
            # a protocol error: `run` reaches the stream error below.
            writer.write(_frame(1, 0, 102, 5_066_666, False, b"late"))

            # A stream error ends the worker's video, naming the stream.
            writer.write(_message(0x84, struct.pack("<IH", 1, 4) + b"boom"))
            with pytest.raises(BridgeError, match="boom"):
                await run
        server.close()

    with pytest.raises(ExceptionGroup) as raised:
        asyncio.run(scenario())
    assert raised.group_contains(BridgeError, match="boom")


def test_the_bridge_going_away_is_an_error(bridge_socket) -> None:
    async def scenario() -> None:
        client, _reader, writer, server = await _connected(bridge_socket)
        writer.close()
        with pytest.raises(BridgeError, match="closed"):
            await client.run()
        server.close()

    asyncio.run(scenario())


def test_frames_for_unknown_streams_are_protocol_errors(bridge_socket) -> None:
    async def scenario() -> None:
        client, _reader, writer, server = await _connected(bridge_socket)
        writer.write(_frame(9, 0, 1, 0, True, b"x"))
        with pytest.raises(BridgeError, match="unknown stream"):
            await client.run()
        server.close()

    asyncio.run(scenario())


def test_an_open_racing_a_bridge_error_fails_instead_of_hanging(bridge_socket) -> None:
    async def scenario() -> None:
        client, reader, writer, server = await _connected(bridge_socket)
        run = asyncio.create_task(client.run())
        opening = asyncio.create_task(client.open("truck1/front", [SD, HD], FakeBroadcast()))
        assert (await _request(reader))[0] == 0x02
        # The bridge fails before it sends the stream's parameter sets.
        writer.write(_message(0x84, struct.pack("<IH", 0, 4) + b"boom"))
        with pytest.raises(BridgeError, match="boom"):
            await run
        # One turn of the loop delivers the failure to the waiting open.
        await asyncio.sleep(0)
        assert opening.done()
        with pytest.raises(BridgeError, match="before the stream was advertised"):
            opening.result()
        # An open after the connection ended fails straight away.
        with pytest.raises(BridgeError, match="has ended"):
            await client.open("truck2/front", [SD], FakeBroadcast())
        server.close()

    asyncio.run(scenario())


def test_finishing_a_stream_twice_closes_it_once(bridge_socket) -> None:
    async def scenario() -> None:
        client, reader, writer, server = await _connected(bridge_socket)
        run = asyncio.create_task(client.run())
        opening = asyncio.create_task(client.open("truck1/front", [SD], FakeBroadcast()))
        assert (await _request(reader))[0] == 0x02
        writer.write(_config(1, 0, b"\x00\x00\x00\x01\x67sd"))
        stream = await opening

        stream.finish()
        stream.finish()
        assert await _request(reader) == (0x05, struct.pack("<I", 1))
        # A keyframe request marks the next message: a second Close would come
        # first, and the bridge answers that by dropping the shared connection.
        client.keyframe(9, 0)
        assert await _request(reader) == (0x04, struct.pack("<IB", 9, 0))
        run.cancel()
        await asyncio.gather(run, return_exceptions=True)
        server.close()

    asyncio.run(scenario())


def test_demand_prepares_rendering_before_the_bridge_and_parks_it_after(bridge_socket) -> None:
    async def scenario() -> None:
        client, reader, writer, server = await _connected(bridge_socket)
        run = asyncio.create_task(client.run())
        broadcast = FakeBroadcast()
        opening = asyncio.create_task(client.open("truck1/front", [SD, HD], broadcast))
        assert (await _request(reader))[0] == 0x02
        writer.write(_config(1, 0, b"\x00\x00\x00\x01\x67sd"))
        writer.write(_config(1, 1, b"\x00\x00\x00\x01\x67hd"))
        stream = await opening

        # The simulator renders nothing in no-rendering mode, so the world is
        # rendering before the bridge is asked for a picture, and stays so
        # until the bridge has been told nobody wants one.
        order: list[str] = []
        sent = client._send
        client._send = lambda message: (order.append(f"demand {message[-1]:02b}"), sent(message))
        demand = asyncio.create_task(stream.watch_demand(lambda wanted: order.append(f"render {wanted}")))
        sd, hd = broadcast.tracks["sd"].demand_handle, broadcast.tracks["hd"].demand_handle
        sd.want(0)
        hd.want(0)
        await _request(reader)
        await _request(reader)
        # A subscriber moving on to the next group is still demand.
        sd.want(1)
        hd.want(None)
        await _request(reader)
        sd.want(None)
        await _request(reader)
        assert order == [
            "render True",
            "demand 01",
            "demand 11",
            "demand 01",
            "demand 00",
            "render False",
        ]
        demand.cancel()
        run.cancel()
        await asyncio.gather(demand, run, return_exceptions=True)
        server.close()

    asyncio.run(scenario())

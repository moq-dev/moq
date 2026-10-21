"""The worker's side of the native CARLA video bridge.

The bridge (`demo/carla/bridge`) runs inside CARLA: it copies each camera's render
target on the GPU, converts and scales it with CUDA, and encodes it with NVENC.
This module speaks its Unix-socket protocol, mirrored from
`demo/carla/bridge/src/protocol.rs`, and republishes the compressed frames on the
truck's broadcast. Raw pixels never reach Python.

Each message is `u32 length | u8 kind | body`: little-endian integers, a string
as `u16 length | UTF-8`, a blob as `u32 length | bytes`.
"""

from __future__ import annotations

import asyncio
import logging
import struct
from dataclasses import dataclass
from datetime import timedelta
from typing import Callable

import moq

log = logging.getLogger(__name__)

VERSION = 1
MAX_MESSAGE = 16 << 20
MAX_RENDITIONS = 8

_HELLO = 0x01
_OPEN = 0x02
_DEMAND = 0x03
_KEYFRAME = 0x04
_CLOSE = 0x05

_EVENT_HELLO = 0x81
_EVENT_CONFIG = 0x82
_EVENT_FRAME = 0x83
_EVENT_ERROR = 0x84


class BridgeError(Exception):
    """The bridge reported a failure, broke the protocol, or went away."""


@dataclass(frozen=True)
class Rendition:
    """One encoded size of a stream, published as track `name`."""

    name: str
    width: int
    height: int
    bitrate: int
    framerate: int


@dataclass(frozen=True)
class Hello:
    version: int


@dataclass(frozen=True)
class Config:
    """A rendition's Annex-B parameter sets, sent before any of its frames."""

    id: int
    rendition: int
    init: bytes


@dataclass(frozen=True)
class Frame:
    """One encoded access unit, stamped with CARLA's frame and simulation time."""

    id: int
    rendition: int
    frame: int
    timestamp_us: int
    keyframe: bool
    payload: bytes


@dataclass(frozen=True)
class Error:
    """Stream 0 is the connection itself; any other stream has stopped."""

    id: int
    message: str


Event = Hello | Config | Frame | Error


def _message(kind: int, body: bytes) -> bytes:
    return struct.pack("<IB", len(body) + 1, kind) + body


def _string(value: str) -> bytes:
    encoded = value.encode()
    return struct.pack("<H", len(encoded)) + encoded


def encode_hello(version: int = VERSION) -> bytes:
    return _message(_HELLO, struct.pack("<H", version))


def encode_open(id: int, name: str, renditions: list[Rendition]) -> bytes:
    body = struct.pack("<I", id) + _string(name) + struct.pack("<B", len(renditions))
    for rendition in renditions:
        body += _string(rendition.name) + struct.pack(
            "<IIQI", rendition.width, rendition.height, rendition.bitrate, rendition.framerate
        )
    return _message(_OPEN, body)


def encode_demand(id: int, renditions: int) -> bytes:
    return _message(_DEMAND, struct.pack("<IB", id, renditions))


def encode_keyframe(id: int, rendition: int) -> bytes:
    return _message(_KEYFRAME, struct.pack("<IB", id, rendition))


def encode_close(id: int) -> bytes:
    return _message(_CLOSE, struct.pack("<I", id))


class _Body:
    def __init__(self, data: bytes) -> None:
        self._data = memoryview(data)
        self._offset = 0

    def take(self, size: int) -> bytes:
        if self._offset + size > len(self._data):
            raise BridgeError(f"truncated message: need {size} bytes, have {len(self._data) - self._offset}")
        value = bytes(self._data[self._offset : self._offset + size])
        self._offset += size
        return value

    def unpack(self, fmt: str) -> tuple[int, ...]:
        return struct.unpack(fmt, self.take(struct.calcsize(fmt)))

    def string(self) -> str:
        (size,) = self.unpack("<H")
        try:
            return self.take(size).decode()
        except UnicodeDecodeError as err:
            raise BridgeError(f"string: {err}") from err

    def blob(self) -> bytes:
        (size,) = self.unpack("<I")
        return self.take(size)

    def end(self) -> None:
        if self._offset != len(self._data):
            raise BridgeError(f"{len(self._data) - self._offset} trailing bytes")


def decode_event(kind: int, body: bytes) -> Event:
    reader = _Body(body)
    event: Event
    if kind == _EVENT_HELLO:
        (version,) = reader.unpack("<H")
        event = Hello(version)
    elif kind == _EVENT_CONFIG:
        id, rendition = reader.unpack("<IB")
        event = Config(id, rendition, reader.blob())
    elif kind == _EVENT_FRAME:
        id, rendition, frame, timestamp_us, keyframe = reader.unpack("<IBQQB")
        if keyframe not in (0, 1):
            raise BridgeError(f"keyframe flag {keyframe}")
        event = Frame(id, rendition, frame, timestamp_us, bool(keyframe), reader.blob())
    elif kind == _EVENT_ERROR:
        (id,) = reader.unpack("<I")
        event = Error(id, reader.string())
    else:
        raise BridgeError(f"unknown event kind {kind:#04x}")
    reader.end()
    return event


async def read_event(reader: asyncio.StreamReader) -> Event | None:
    """The next event, or None when the bridge closed the socket between messages."""
    try:
        header = await reader.readexactly(4)
    except asyncio.IncompleteReadError as err:
        if err.partial:
            raise BridgeError("bridge closed mid-message") from err
        return None
    (length,) = struct.unpack("<I", header)
    if length == 0 or length > MAX_MESSAGE:
        raise BridgeError(f"message length {length}")
    try:
        message = await reader.readexactly(length)
    except asyncio.IncompleteReadError as err:
        raise BridgeError("bridge closed mid-message") from err
    return decode_event(message[0], message[1:])


class Bridge:
    """One connection to the bridge, shared by every truck's stream.

    `run` is the only reader: it must be running while streams open, since the
    parameter sets each `open` waits for arrive through it.
    """

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self._reader = reader
        self._writer = writer
        self._streams: dict[int, BridgeStream] = {}
        # Ids are never reused, so a closed one stays known: the bridge may
        # still deliver frames it encoded before it saw the close.
        self._closed: set[int] = set()
        self._next_id = 1
        # Why `run` ended; every stream opened after that fails with it.
        self._failure: BaseException | None = None

    @classmethod
    async def connect(cls, path: str) -> Bridge:
        reader, writer = await asyncio.open_unix_connection(path, limit=MAX_MESSAGE)
        bridge = cls(reader, writer)
        bridge._send(encode_hello())
        hello = await read_event(reader)
        if hello != Hello(VERSION):
            raise BridgeError(f"expected hello {VERSION}, got {hello!r}")
        return bridge

    def disconnect(self) -> None:
        """Close the connection; the bridge stops every stream on it."""
        self._writer.close()

    def _send(self, message: bytes) -> None:
        # Control messages are tiny and rare; the transport buffers them.
        self._writer.write(message)

    async def open(self, name: str, renditions: list[Rendition], broadcast: moq.BroadcastProducer) -> BridgeStream:
        """Encode the cameras registered as `name` into `broadcast`'s tracks.

        Returns once every rendition is advertised, before anyone subscribes.
        """
        if not renditions or len(renditions) > MAX_RENDITIONS:
            raise ValueError(f"{name}: {len(renditions)} renditions")
        if self._failure is not None:
            raise BridgeError(f"{name}: the bridge connection has ended") from self._failure
        id = self._next_id
        self._next_id += 1
        stream = BridgeStream(self, id, name, renditions, broadcast)
        self._streams[id] = stream
        self._send(encode_open(id, name, renditions))
        await stream.advertised()
        return stream

    async def run(self) -> None:
        """Deliver events until the connection ends. Always raises: the bridge
        going away, or any error it reports, ends the worker's video."""
        try:
            await self._deliver()
        except BaseException as err:
            # Nothing else answers an `open` still waiting for its parameter
            # sets, so it would wait forever; later ones fail straight away.
            self._failure = err
            for stream in self._streams.values():
                stream.fail(err)
            raise

    async def _deliver(self) -> None:
        while True:
            event = await read_event(self._reader)
            if event is None:
                raise BridgeError("bridge closed the connection")
            if isinstance(event, Error):
                stream = self._streams.get(event.id)
                where = stream.name if stream else "bridge"
                raise BridgeError(f"{where}: {event.message}")
            if isinstance(event, Hello):
                raise BridgeError("unexpected hello")
            stream = self._streams.get(event.id)
            if stream is None:
                if event.id in self._closed:
                    continue
                raise BridgeError(f"event for unknown stream {event.id}")
            if isinstance(event, Config):
                stream.configure(event)
            else:
                stream.publish(event)

    def demand(self, id: int, renditions: int) -> None:
        self._send(encode_demand(id, renditions))

    def keyframe(self, id: int, rendition: int) -> None:
        self._send(encode_keyframe(id, rendition))

    def close(self, id: int) -> None:
        self._streams.pop(id, None)
        self._closed.add(id)
        self._send(encode_close(id))


class BridgeStream:
    """One broadcast's video tracks, fed by the bridge.

    Each rendition's subscribers drive the bridge's demand, which decides what
    CARLA renders: the smallest camera covering the largest demanded rendition,
    scaled on the GPU for the rest, and nothing at all without subscribers.
    """

    def __init__(
        self,
        bridge: Bridge,
        id: int,
        name: str,
        renditions: list[Rendition],
        broadcast: moq.BroadcastProducer,
    ) -> None:
        self._bridge = bridge
        self.id = id
        self.name = name
        self._renditions = renditions
        self._broadcast = broadcast
        self._tracks: list[moq.media.TrackProducer | None] = [None] * len(renditions)
        self._advertised: asyncio.Future[None] = asyncio.get_running_loop().create_future()
        self._demand = 0
        self._on_demand: Callable[[bool], None] | None = None
        self._finished = False
        # Simulation time of the first frame, shared by every rendition so their
        # timelines line up.
        self._origin_us: int | None = None
        self.published = [0] * len(renditions)

    async def advertised(self) -> None:
        await self._advertised

    @property
    def rendition_names(self) -> list[str]:
        return [rendition.name for rendition in self._renditions]

    def fail(self, err: BaseException) -> None:
        """End a pending `advertised` with the connection's failure."""
        if not self._advertised.done():
            failure = BridgeError(f"{self.name}: the bridge connection ended before the stream was advertised")
            failure.__cause__ = err
            self._advertised.set_exception(failure)

    def configure(self, config: Config) -> None:
        if config.rendition >= len(self._renditions) or self._tracks[config.rendition] is not None:
            raise BridgeError(f"{self.name}: unexpected config for rendition {config.rendition}")
        rendition = self._renditions[config.rendition]
        # The parameter sets resolve the catalog entry up front, so a viewer can
        # subscribe before the first frame exists, which is what starts it.
        self._tracks[config.rendition] = moq.media.TrackProducer.video(
            self._broadcast,
            moq.media.VideoInit(
                format=moq.media.VideoFormat.AVC3,
                data=config.init,
                hint=moq.media.VideoHint(
                    coded=moq.media.Dimensions(width=rendition.width, height=rendition.height),
                    bitrate=rendition.bitrate,
                    framerate=float(rendition.framerate),
                ),
            ),
            target=moq.media.Named(rendition.name),
        )
        if all(track is not None for track in self._tracks) and not self._advertised.done():
            self._advertised.set_result(None)

    def publish(self, frame: Frame) -> None:
        if frame.rendition >= len(self._tracks):
            raise BridgeError(f"{self.name}: frame for rendition {frame.rendition}")
        track = self._tracks[frame.rendition]
        if track is None:
            raise BridgeError(f"{self.name}: frame before config for rendition {frame.rendition}")
        if self._origin_us is None:
            self._origin_us = frame.timestamp_us
        timestamp = timedelta(microseconds=max(0, frame.timestamp_us - self._origin_us))
        track.write_frame(frame.payload, timestamp)
        # Live encoder output: records the handoff for the catalog's jitter.
        track.flush(timestamp)
        self.published[frame.rendition] += 1

    async def watch_demand(self, on_demand: Callable[[bool], None] | None = None) -> None:
        """Forward each rendition's subscribers to the bridge until cancelled.

        `on_demand` hears whether any rendition is wanted, before the bridge
        starts rendering and after it stops, so what it prepares (the
        simulator's renderer) is ready for the first picture.
        """
        self._on_demand = on_demand
        async with asyncio.TaskGroup() as group:
            for index, track in enumerate(self._tracks):
                assert track is not None, "watch_demand before advertised"
                group.create_task(self._watch(index, track.demand()), name=f"demand:{self.name}:{index}")

    async def _watch(self, index: int, demand: moq.TrackDemand) -> None:
        while True:
            while await demand.wanted_changed() is None:
                pass
            self._set_demand(index, True)
            try:
                while await demand.wanted_changed() is not None:
                    pass
            finally:
                self._set_demand(index, False)

    def _set_demand(self, index: int, used: bool) -> None:
        # A watcher cancelled before `finish` may still clear its bit; the bridge
        # would refuse demand for a closed stream and drop the connection.
        if self._finished:
            return
        mask = self._demand | (1 << index) if used else self._demand & ~(1 << index)
        if mask == self._demand:
            return
        # A failed `on_demand` commits nothing and fails the watcher.
        if not self._demand and self._on_demand is not None:
            self._on_demand(True)
        self._demand = mask
        self._bridge.demand(self.id, mask)
        if not mask and self._on_demand is not None:
            self._on_demand(False)

    def keyframe(self, rendition: int) -> None:
        self._bridge.keyframe(self.id, rendition)

    def finish(self) -> None:
        """Stop encoding and end the tracks cleanly for subscribers. Idempotent:
        the bridge refuses a second Close, which would drop the connection
        every truck's video shares."""
        if self._finished:
            return
        self._finished = True
        self._bridge.close(self.id)
        for track in self._tracks:
            if track is None:
                continue
            try:
                track.finish()
            # uniffi's stubs declare the error a plain class; at runtime it is an Exception.
            except moq.Error as err:  # pyright: ignore[reportGeneralTypeIssues]
                log.debug("%s: finishing a video track: %s", self.name, err)


__all__ = [
    "Bridge",
    "BridgeError",
    "BridgeStream",
    "Config",
    "Error",
    "Frame",
    "Hello",
    "Rendition",
    "decode_event",
    "encode_close",
    "encode_demand",
    "encode_hello",
    "encode_keyframe",
    "encode_open",
    "read_event",
]

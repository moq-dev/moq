"""Native video encoding, publication, and decoding."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from moq_ffi import (
    MoqVideoDecoderOutput,
    MoqVideoEncoder,
    MoqVideoFrame,
    MoqVideoPacket,
    MoqVideoProducer,
    moq_video_decode,
)

from ._records import _from_us, _opt_us, _to_us
from .media import CatalogProducer, Dimensions, Video
from .publish import BroadcastProducer, TrackDemand
from .session import Bandwidth, Reservation
from .subscribe import BroadcastConsumer
from .subscribe import VideoDecoder as Decoder
from .types import VideoEncoderInput as EncoderInput
from .types import VideoEncoderOutput as EncoderOutput
from .types import VideoGop as Gop


@dataclass(frozen=True)
class Frame:
    timestamp: timedelta
    data: bytes

    def _ffi(self) -> MoqVideoFrame:
        return MoqVideoFrame(timestamp_us=_to_us(self.timestamp, "timestamp"), data=self.data)


@dataclass(frozen=True)
class Packet:
    timestamp: timedelta
    data: bytes
    keyframe: bool

    @staticmethod
    def _from_ffi(value: MoqVideoPacket) -> Packet:
        return Packet(timestamp=_from_us(value.timestamp_us), data=value.data, keyframe=value.keyframe)


@dataclass(frozen=True)
class DecoderOutput:
    resize: Dimensions | None = None
    max_delay: timedelta | None = None
    surface: bool = False

    def _ffi(self) -> MoqVideoDecoderOutput:
        return MoqVideoDecoderOutput(
            resize=self.resize,
            max_delay_us=_opt_us(self.max_delay, "max_delay"),
            surface=self.surface,
        )


class Producer:
    def __init__(self, inner: MoqVideoProducer) -> None:
        self._inner = inner

    def demand(self) -> TrackDemand:
        return TrackDemand(self._inner.demand())

    def write(self, frame: Frame) -> None:
        self._inner.write(frame._ffi())

    def cut(self) -> None:
        self._inner.cut()

    def set_bitrate(self, bitrate: int) -> None:
        self._inner.set_bitrate(bitrate)

    def reservation(self) -> Reservation | None:
        value = self._inner.reservation()
        return None if value is None else Reservation(value)

    def finish(self) -> None:
        self._inner.finish()


def producer(
    broadcast: BroadcastProducer,
    catalog: CatalogProducer,
    input: EncoderInput,
    output: EncoderOutput,
    *,
    bandwidth: Bandwidth | None = None,
) -> Producer:
    """Encode raw pictures and publish them into a broadcast's catalog."""
    return Producer(
        MoqVideoProducer(
            broadcast._inner,
            catalog._inner,
            input,
            output,
            None if bandwidth is None else bandwidth._inner,
        )
    )


class Encoder:
    """Encode raw pictures without publishing them."""

    def __init__(self, input: EncoderInput, output: EncoderOutput) -> None:
        self._inner = MoqVideoEncoder(input, output)

    def encode(self, frame: Frame) -> list[Packet]:
        """Encode one picture, returning zero or more access units."""
        return [Packet._from_ffi(value) for value in self._inner.encode(frame._ffi())]

    def cut(self) -> None:
        """Start a group at the next encoded picture."""
        self._inner.cut()

    def set_bitrate(self, bitrate: int) -> None:
        """Retune the live encoder in bits per second."""
        self._inner.set_bitrate(bitrate)

    def flush(self) -> list[Packet]:
        """Drain buffered pictures while keeping the encoder open."""
        return [Packet._from_ffi(value) for value in self._inner.flush()]

    def finish(self) -> list[Packet]:
        """Drain and close the encoder."""
        return [Packet._from_ffi(value) for value in self._inner.finish()]


async def decoder(
    broadcast: BroadcastConsumer,
    name: str,
    rendition: Video,
    output: DecoderOutput | None = None,
) -> Decoder:
    """Subscribe to and decode one catalog video rendition."""
    selected = output or DecoderOutput()
    return Decoder(await moq_video_decode(broadcast._inner, name, rendition, selected._ffi()))


__all__ = [
    "Decoder",
    "DecoderOutput",
    "Encoder",
    "EncoderInput",
    "EncoderOutput",
    "Frame",
    "Gop",
    "Packet",
    "Producer",
    "decoder",
    "producer",
]

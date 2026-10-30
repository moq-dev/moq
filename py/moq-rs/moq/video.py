"""Native video encoding, publication, and decoding."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta
from typing import Union

from moq_ffi import (
    MoqVideoConsumer,
    MoqVideoDecoderOutput,
    MoqVideoEncoder,
    MoqVideoFrame,
    MoqVideoPacket,
    MoqVideoProducer,
    MoqVideoProducerOptions,
    MoqVideoRead,
)
from moq_ffi import MoqVideoFrameRate as FrameRate

from ._read import End, Gap, _end, _gap
from ._records import _from_us, _to_us
from ._stream import _Stream
from .media import CatalogProducer, DecodeStart, Dimensions, Video
from .publish import TrackDemand
from .session import Allocator, Reservation
from .subscribe import BroadcastConsumer
from .types import VideoCodec as Codec
from .types import VideoDecodedFrame
from .types import VideoEncoderInput as EncoderInput
from .types import VideoEncoderKind as EncoderKind
from .types import VideoEncoderOutput as EncoderOutput
from .types import VideoGop as Gop
from .types import VideoPixelFormat as PixelFormat

# A Union rather than `|`: the generated frame class does not support `|` with other types.
Read = Union[VideoDecodedFrame, Gap, End]
"""One item a :class:`Consumer` reads, in delivery order: a decoded picture, or a break between them."""


@dataclass(frozen=True)
class Frame:
    """One raw picture in the configured input layout, with its presentation timestamp."""

    timestamp: timedelta
    data: bytes

    def _ffi(self) -> MoqVideoFrame:
        return MoqVideoFrame(timestamp_us=_to_us(self.timestamp, "timestamp"), data=self.data)


@dataclass(frozen=True)
class Packet:
    """One encoded access unit from an :class:`Encoder`."""

    timestamp: timedelta
    data: bytes
    keyframe: bool

    @staticmethod
    def _from_ffi(value: MoqVideoPacket) -> Packet:
        return Packet(timestamp=_from_us(value.timestamp_us), data=value.data, keyframe=value.keyframe)


@dataclass(frozen=True)
class DecoderOutput:
    """What a :class:`Consumer` delivers: an optional resize, a max delay, and the decoder's surface (macOS only).

    ``start`` picks where to begin on a track that already holds groups, ``None`` for the
    oldest: a live player passes ``DecodeStart.LATEST`` so a reopened decoder skips the
    cached backlog.
    """

    resize: Dimensions | None = None
    max_delay: timedelta | None = None
    surface: bool = False
    start: DecodeStart | None = None

    def _ffi(self) -> MoqVideoDecoderOutput:
        return MoqVideoDecoderOutput(
            resize=self.resize,
            max_delay=self.max_delay,
            surface=self.surface,
            start=self.start,
        )


class Producer:
    """Publish raw pictures, encoded on the way out. Construct it from the broadcast's catalog.

    Pixel format, resolution, and framerate are fixed at construction; each
    :meth:`write` passes only pixels and a presentation timestamp.
    """

    def __init__(
        self,
        catalog: CatalogProducer,
        input: EncoderInput,
        output: EncoderOutput,
        *,
        track: str | None = None,
        bandwidth: Allocator | None = None,
        lower: Producer | None = None,
    ) -> None:
        """Open a video track on the catalog's broadcast and register its rendition.

        Pass ``lower``, the next rendition down a simulcast ladder, to disable this
        one while the bandwidth grant cannot fund it; it resumes once the grant
        recovers. Needs ``bandwidth`` and a bitrate above the lower rendition's.
        """
        self._inner = MoqVideoProducer(
            catalog._inner,
            MoqVideoProducerOptions(
                input=input,
                output=output,
                track=track,
                bandwidth=None if bandwidth is None else bandwidth._inner,
                lower=None if lower is None else lower._inner,
            ),
        )

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this video track has subscribers."""
        return TrackDemand(self._inner.demand())

    def write(self, frame: Frame) -> None:
        """Encode and publish one picture in the configured input format.

        A hardware encoder pipelines, so a call that puts nothing on the wire
        is normal rather than an error.
        """
        self._inner.write(frame._ffi())

    def cut(self) -> None:
        """Start a new group at the next written frame.

        Optional: the encoder starts groups on its own per ``output.gop``. Raises
        if the selected encoder cannot force a keyframe; groups keep their interval.
        """
        self._inner.cut()

    def set_bitrate(self, bitrate: int) -> None:
        """Retune the live encoder, in bits per second, without forcing a keyframe.

        Raises if this backend cannot retune while running; the encoder keeps its rate.
        """
        self._inner.set_bitrate(bitrate)

    def reservation(self) -> Reservation | None:
        """This encoder's bandwidth reservation, if published against a session allocator."""
        value = self._inner.reservation()
        return None if value is None else Reservation(value)

    def finish(self) -> None:
        """Flush any frames the codec is holding and finalize the track."""
        self._inner.finish()


class Consumer(_Stream[Read]):
    """Async iterator of decoded frames, and the :class:`Gap` and :class:`End` breaks
    between them. Construct with :meth:`subscribe`.

    Each frame owns its decoded picture until it is garbage collected;
    ``frame.pixels(format)`` converts it to tightly packed CPU pixels.
    ``output.resize`` is best effort, so read each frame's ``width()`` and ``height()``.
    """

    @classmethod
    async def subscribe(
        cls, broadcast: BroadcastConsumer, id: str, rendition: Video, output: DecoderOutput | None = None
    ) -> Consumer:
        """Subscribe to the catalog video rendition ``id`` and decode it."""
        selected = output or DecoderOutput()
        return cls(await MoqVideoConsumer.subscribe(broadcast._inner, id, rendition, selected._ffi()))

    def __init__(self, inner: MoqVideoConsumer) -> None:
        self._inner = inner

    async def _next(self) -> Read | None:
        value = await self._inner.next()
        if value is None:
            return None
        if isinstance(value, MoqVideoRead.FRAME):
            return value.frame
        if isinstance(value, MoqVideoRead.GAP):
            return _gap(value.timestamp_us, value.duration)
        if isinstance(value, MoqVideoRead.END):
            return _end(value.end_us)
        raise TypeError(f"unknown video read: {value!r}")

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering video frames."""
        self._inner.cancel()


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


__all__ = [
    "Codec",
    "Consumer",
    "DecoderOutput",
    "Encoder",
    "EncoderInput",
    "EncoderKind",
    "EncoderOutput",
    "End",
    "Frame",
    "Gap",
    "Gop",
    "Packet",
    "PixelFormat",
    "Producer",
    "Read",
    "FrameRate",
]

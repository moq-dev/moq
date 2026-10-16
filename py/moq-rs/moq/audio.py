"""Native audio encoding, publication, and decoding."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from moq_ffi import (
    MoqAudioDecoderOutput,
    MoqAudioEncoder,
    MoqAudioEncoderFinish,
    MoqAudioEncoderOutput,
    MoqAudioFrame,
    MoqAudioPacket,
    MoqAudioProducer,
    moq_audio_decode,
)

from ._records import _from_us, _opt_us, _to_us
from .media import Audio, CatalogProducer
from .publish import BroadcastProducer, TrackDemand
from .session import Bandwidth, Reservation
from .subscribe import BroadcastConsumer
from .types import AudioCodec as Codec
from .types import AudioEncoderInput as EncoderInput
from .types import AudioSampleFormat as SampleFormat


@dataclass(frozen=True)
class Frame:
    """Interleaved PCM with its presentation timestamp."""

    timestamp: timedelta
    data: bytes

    def _ffi(self) -> MoqAudioFrame:
        return MoqAudioFrame(timestamp_us=_to_us(self.timestamp, "timestamp"), data=self.data)

    @staticmethod
    def _from_ffi(value: MoqAudioFrame) -> Frame:
        return Frame(timestamp=_from_us(value.timestamp_us), data=value.data)


@dataclass(frozen=True)
class EncoderOutput:
    codec: Codec
    track: str | None = None
    sample_rate: int | None = None
    channels: int | None = None
    bitrate: int | None = None
    frame_duration: timedelta = timedelta(0)

    def _ffi(self) -> MoqAudioEncoderOutput:
        return MoqAudioEncoderOutput(
            codec=self.codec,
            track=self.track,
            sample_rate=self.sample_rate,
            channels=self.channels,
            bitrate=self.bitrate,
            frame_duration_us=_to_us(self.frame_duration, "frame_duration"),
        )


@dataclass(frozen=True)
class DecoderOutput:
    format: SampleFormat
    sample_rate: int | None = None
    channels: int | None = None
    max_delay: timedelta | None = None

    def _ffi(self) -> MoqAudioDecoderOutput:
        return MoqAudioDecoderOutput(
            format=self.format,
            sample_rate=self.sample_rate,
            channels=self.channels,
            max_delay_us=_opt_us(self.max_delay, "max_delay"),
        )


class Producer:
    def __init__(self, inner: MoqAudioProducer) -> None:
        self._inner = inner

    def demand(self) -> TrackDemand:
        return TrackDemand(self._inner.demand())

    def reanchor(self) -> None:
        self._inner.reanchor()

    def write(self, frame: Frame) -> None:
        self._inner.write(frame._ffi())

    def reservation(self) -> Reservation | None:
        value = self._inner.reservation()
        return None if value is None else Reservation(value)

    def finish(self) -> None:
        self._inner.finish()


class Decoder:
    def __init__(self, inner) -> None:
        self._inner = inner

    def __aiter__(self):
        return self

    async def __anext__(self) -> Frame:
        value = await self._inner.next()
        if value is None:
            raise StopAsyncIteration
        return Frame._from_ffi(value)

    def cancel(self) -> None:
        self._inner.cancel()


def producer(
    broadcast: BroadcastProducer,
    catalog: CatalogProducer,
    input: EncoderInput,
    output: EncoderOutput,
    *,
    bandwidth: Bandwidth | None = None,
) -> Producer:
    """Encode PCM and publish it into a broadcast's catalog."""
    return Producer(
        MoqAudioProducer(
            broadcast._inner,
            catalog._inner,
            input,
            output._ffi(),
            None if bandwidth is None else bandwidth._inner,
        )
    )


class Encoder:
    """Encode codec-sized interleaved f32 PCM without publishing it."""

    def __init__(self, output: EncoderOutput) -> None:
        self._inner = MoqAudioEncoder(output._ffi())

    @property
    def frame_size(self) -> int:
        """Samples per channel required by each encode call."""
        return self._inner.frame_size()

    def encode(self, pcm: list[float]) -> MoqAudioPacket:
        """Encode one full PCM frame."""
        return self._inner.encode(pcm)

    def finish(self, pcm: list[float] | None = None) -> MoqAudioEncoderFinish:
        """Pad an optional partial frame and drain the codec."""
        return self._inner.finish(pcm or [])


async def decoder(
    broadcast: BroadcastConsumer,
    name: str,
    rendition: Audio,
    output: DecoderOutput,
) -> Decoder:
    """Subscribe to and decode one catalog audio rendition."""
    return Decoder(await moq_audio_decode(broadcast._inner, name, rendition, output._ffi()))


__all__ = [
    "Decoder",
    "DecoderOutput",
    "Encoder",
    "EncoderInput",
    "EncoderOutput",
    "Frame",
    "Codec",
    "SampleFormat",
    "Producer",
    "decoder",
    "producer",
]

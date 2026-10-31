"""Native audio encoding, publication, and decoding."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from moq_ffi import (
    MoqAudioConsumer,
    MoqAudioDecoderOutput,
    MoqAudioEncoder,
    MoqAudioEncoderFinished,
    MoqAudioEncoderOutput,
    MoqAudioFrame,
    MoqAudioPacket,
    MoqAudioProducer,
    MoqAudioProducerOptions,
    MoqAudioRead,
)

from ._read import End, Gap, _end, _gap
from ._records import _from_us, _to_us
from ._stream import _Stream
from .media import Audio, AudioInit, CatalogProducer, DecodeStart
from .publish import TrackDemand
from .session import Allocator, Reservation
from .subscribe import BroadcastConsumer
from .types import AudioCodec as Codec
from .types import AudioEncoderInput as EncoderInput
from .types import AudioSampleFormat as SampleFormat

Packet = MoqAudioPacket
"""One encoded payload from an :class:`Encoder`."""

EncoderFinish = MoqAudioEncoderFinished
"""The packets :meth:`Encoder.finish` drains and the padded samples to discard."""


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


Read = Frame | Gap | End
"""One item a :class:`Consumer` reads, in delivery order: decoded PCM, or a break in it."""


@dataclass(frozen=True)
class EncoderOutput:
    """Codec-side settings.

    ``frame_duration`` of ``None`` takes the codec's own frame: 20 ms for Opus,
    1024 samples for AAC. 10 ms Opus packets halve the packetization delay.
    """

    codec: Codec
    sample_rate: int | None = None
    channels: int | None = None
    bitrate: int | None = None
    frame_duration: timedelta | None = None

    def _ffi(self) -> MoqAudioEncoderOutput:
        return MoqAudioEncoderOutput(
            codec=self.codec,
            sample_rate=self.sample_rate,
            channels=self.channels,
            bitrate=self.bitrate,
            frame_duration=self.frame_duration,
        )


@dataclass(frozen=True)
class DecoderOutput:
    """The PCM layout a :class:`Consumer` delivers, how stale a group may get, and its playout delay.

    ``delay`` fixes the jitter buffer to hold ahead of the playhead; ``None``
    estimates it from arrival timing. ``max_delay`` only bounds how stale a
    group may get before it is skipped, and adds no buffering. ``start`` picks
    where to begin on a track that already holds groups, ``None`` for the oldest:
    a live player passes ``DecodeStart.LATEST`` so a reopened decoder skips the
    cached backlog.
    """

    format: SampleFormat
    sample_rate: int | None = None
    channels: int | None = None
    max_delay: timedelta | None = None
    delay: timedelta | None = None
    start: DecodeStart | None = None

    def _ffi(self) -> MoqAudioDecoderOutput:
        return MoqAudioDecoderOutput(
            format=self.format,
            sample_rate=self.sample_rate,
            channels=self.channels,
            max_delay=self.max_delay,
            delay=self.delay,
            start=self.start,
        )


class Producer:
    """Publish raw PCM, encoded on the way out. Construct it from the broadcast's catalog.

    The PCM layout, codec, bitrate, and frame duration are fixed at
    construction; each :meth:`write` passes only samples and a timestamp.
    """

    def __init__(
        self,
        catalog: CatalogProducer,
        input: EncoderInput,
        output: EncoderOutput,
        *,
        track: str | None = None,
        bandwidth: Allocator | None = None,
    ) -> None:
        """Open an audio track on the catalog's broadcast and register its rendition."""
        self._inner = MoqAudioProducer(
            catalog._inner,
            MoqAudioProducerOptions(
                input=input,
                output=output._ffi(),
                track=track,
                bandwidth=None if bandwidth is None else bandwidth._inner,
            ),
        )

    def demand(self) -> TrackDemand:
        """A watch-only handle to whether this audio track has subscribers."""
        return TrackDemand(self._inner.demand())

    def reanchor(self) -> None:
        """Re-anchor the timeline to the next frame after an idle gap."""
        self._inner.reanchor()

    def write(self, frame: Frame) -> None:
        """Push one frame of PCM in the configured input format."""
        self._inner.write(frame._ffi())

    def reservation(self) -> Reservation | None:
        """This encoder's bandwidth reservation, if published against a session allocator."""
        value = self._inner.reservation()
        return None if value is None else Reservation(value)

    def finish(self) -> None:
        """Flush any pending samples and finalize the track."""
        self._inner.finish()


class Consumer(_Stream[Read]):
    """Async iterator of decoded PCM :class:`Frame` reads, and the :class:`Gap` and
    :class:`End` breaks between them. Construct with :meth:`subscribe`.

    The layout is fixed by the :class:`DecoderOutput` passed at subscribe time;
    each frame's ``data`` is raw bytes in that format. Restart playback after a
    gap or an end, since what follows does not continue what came before.
    """

    @classmethod
    async def subscribe(
        cls, broadcast: BroadcastConsumer, id: str, rendition: Audio, output: DecoderOutput
    ) -> Consumer:
        """Subscribe to the catalog audio rendition ``id`` and decode it."""
        return cls(await MoqAudioConsumer.subscribe(broadcast._inner, id, rendition, output._ffi()))

    def __init__(self, inner: MoqAudioConsumer) -> None:
        self._inner = inner

    async def _next(self) -> Read | None:
        value = await self._inner.next()
        if value is None:
            return None
        if isinstance(value, MoqAudioRead.FRAME):
            return Frame._from_ffi(value.frame)
        if isinstance(value, MoqAudioRead.GAP):
            return _gap(value.timestamp_us, value.duration)
        if isinstance(value, MoqAudioRead.END):
            return _end(value.end_us)
        raise TypeError(f"unknown audio read: {value!r}")

    @property
    def delay(self) -> timedelta:
        """The playout delay to hold ahead of the playhead: the fixed one, or the estimate so far.

        The estimate measures when each frame is read, so read the next frame as
        soon as the previous one is handled rather than pacing reads to the speaker.
        """
        return self._inner.delay()

    def cancel(self) -> None:
        """Cancel the subscription and stop delivering audio frames."""
        self._inner.cancel()


class Encoder:
    """Encode codec-sized interleaved f32 PCM without publishing it."""

    def __init__(self, output: EncoderOutput) -> None:
        self._inner = MoqAudioEncoder(output._ffi())

    @property
    def frame_size(self) -> int:
        """Samples per channel required by each encode call."""
        return self._inner.frame_size()

    def track_init(self) -> AudioInit:
        """The OpusHead or AudioSpecificConfig that publishes these packets via :meth:`TrackProducer.audio`."""
        return self._inner.track_init()

    def set_bitrate(self, bitrate: int) -> None:
        """Retune the live encoder in bits per second."""
        self._inner.set_bitrate(bitrate)

    def encode(self, pcm: list[float]) -> Packet:
        """Encode one full PCM frame."""
        return self._inner.encode(pcm)

    def finish(self, pcm: list[float] | None = None) -> EncoderFinish:
        """Pad an optional partial frame and drain the codec."""
        return self._inner.finish(pcm or [])


__all__ = [
    "Consumer",
    "DecoderOutput",
    "Encoder",
    "EncoderInput",
    "EncoderFinish",
    "EncoderOutput",
    "End",
    "Frame",
    "Gap",
    "Codec",
    "Packet",
    "Producer",
    "Read",
    "SampleFormat",
]

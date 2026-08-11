"""CMAF packaging for application-selected encoded media frames."""

from dataclasses import dataclass
from typing import Sequence, cast

from moq_ffi import (
    MoqAudio,
    MoqCmafConfig,
    MoqCmafMuxer,
    MoqCmafTrack,
    MoqMediaFrame,
    MoqVideo,
)


@dataclass(frozen=True)
class CmafOutput:
    """Initialization and media data produced for one frame batch."""

    initialization: bytes | None
    """The current initialization segment once codec metadata is available."""

    fragment: bytes | None
    """The media fragment, or ``None`` when the batch produced no usable samples."""


class CmafMuxer:
    """Package one encoded audio or video rendition as CMAF."""

    def __init__(self, track: MoqVideo | MoqAudio, *, origin_us: int = 0) -> None:
        """Create a muxer whose output timestamps are relative to ``origin_us``."""
        if isinstance(track, MoqVideo):
            ffi_track = MoqCmafTrack.VIDEO(config=track)
        elif isinstance(track, MoqAudio):
            ffi_track = MoqCmafTrack.AUDIO(config=track)
        else:
            raise TypeError("track must be a Video or Audio")
        self._ffi = MoqCmafMuxer(MoqCmafConfig(track=cast(MoqCmafTrack, ffi_track), origin_us=origin_us))

    @property
    def initialization(self) -> bytes | None:
        """Return the current initialization, or ``None`` until inline metadata arrives."""
        return self._ffi.init_segment()

    def mux(self, sequence: int, frames: Sequence[MoqMediaFrame]) -> CmafOutput:
        """Encode one codec-configuration-consistent batch on the configured timeline."""
        output = self._ffi.mux(sequence, list(frames))
        return CmafOutput(
            initialization=output.initialization,
            fragment=output.fragment,
        )

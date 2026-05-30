"""Re-export moq-ffi record types without the Moq prefix."""

from moq_ffi import (
    MoqAudio as Audio,
)
from moq_ffi import (
    MoqAudioCodec as AudioCodec,
)
from moq_ffi import (
    MoqAudioDecoderOutput as AudioDecoderOutput,
)
from moq_ffi import (
    MoqAudioEncoderInput as AudioEncoderInput,
)
from moq_ffi import (
    MoqAudioEncoderOutput as AudioEncoderOutput,
)
from moq_ffi import (
    MoqAudioFormat as AudioFormat,
)
from moq_ffi import (
    MoqAudioFrame as AudioFrame,
)
from moq_ffi import (
    MoqCatalog as Catalog,
)
from moq_ffi import (
    MoqDimensions as Dimensions,
)
from moq_ffi import (
    MoqFrame as Frame,
)
from moq_ffi import (
    MoqVideo as Video,
)

__all__ = [
    "Audio",
    "AudioCodec",
    "AudioDecoderOutput",
    "AudioEncoderInput",
    "AudioEncoderOutput",
    "AudioFormat",
    "AudioFrame",
    "Catalog",
    "Dimensions",
    "Frame",
    "Video",
]

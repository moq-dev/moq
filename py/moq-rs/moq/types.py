"""Re-export moq-ffi record types without the Moq prefix."""

from ._uniffi import (
    MoqAudio as Audio,
)
from ._uniffi import (
    MoqAudioCodec as AudioCodec,
)
from ._uniffi import (
    MoqAudioDecoderConfig as AudioDecoderConfig,
)
from ._uniffi import (
    MoqAudioEncoderConfig as AudioEncoderConfig,
)
from ._uniffi import (
    MoqAudioFormat as AudioFormat,
)
from ._uniffi import (
    MoqAudioFrame as AudioFrame,
)
from ._uniffi import (
    MoqCatalog as Catalog,
)
from ._uniffi import (
    MoqDimensions as Dimensions,
)
from ._uniffi import (
    MoqFrame as Frame,
)
from ._uniffi import (
    MoqVideo as Video,
)

__all__ = [
    "Audio",
    "AudioCodec",
    "AudioDecoderConfig",
    "AudioEncoderConfig",
    "AudioFormat",
    "AudioFrame",
    "Catalog",
    "Dimensions",
    "Frame",
    "Video",
]

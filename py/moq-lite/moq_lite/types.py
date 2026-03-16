"""Re-export moq-ffi record types without the Moq prefix."""

from moq_ffi import (
    MoqAudio as Audio,
    MoqCatalog as Catalog,
    MoqDimensions as Dimensions,
    MoqFrame as Frame,
    MoqVideo as Video,
)

__all__ = ["Audio", "Catalog", "Dimensions", "Frame", "Video"]

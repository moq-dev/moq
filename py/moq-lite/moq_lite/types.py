"""Re-export moq-ffi record types without the Moq prefix."""

from moq_ffi import (
    MoqAudio as Audio,
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

__all__ = ["Audio", "Catalog", "Dimensions", "Frame", "Video"]

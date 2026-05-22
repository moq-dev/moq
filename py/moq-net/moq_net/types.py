"""Re-export moq-ffi record types without the Moq prefix."""

from ._uniffi import (
    MoqAudio as Audio,
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

__all__ = ["Audio", "Catalog", "Dimensions", "Frame", "Video"]

"""Re-export moq-ffi record types without the Moq prefix.

Records with duration fields are owned in `_records` instead, so they read as `timedelta`.
"""

from moq_ffi import (
    MoqAnchor as Anchor,
)
from moq_ffi import (
    MoqAnnounceRoute as AnnounceRoute,
)
from moq_ffi import (
    MoqAudioCodec as AudioCodec,
)
from moq_ffi import (
    MoqAudioEncoderInput as AudioEncoderInput,
)
from moq_ffi import (
    MoqAudioPreset as AudioPreset,
)
from moq_ffi import (
    MoqAudioSampleFormat as AudioSampleFormat,
)
from moq_ffi import (
    MoqConnectionStatus as ConnectionStatus,
)
from moq_ffi import (
    MoqErrorScope as ErrorScope,
)
from moq_ffi import (
    MoqFetchGroupOptions as FetchGroupOptions,
)
from moq_ffi import (
    MoqOrder as Order,
)
from moq_ffi import (
    MoqProtocolError as ProtocolError,
)
from moq_ffi import (
    MoqProtocolKind as ProtocolKind,
)
from moq_ffi import (
    MoqRange as Range,
)
from moq_ffi import (
    MoqRoute as Route,
)
from moq_ffi import (
    MoqVideoCodec as VideoCodec,
)
from moq_ffi import (
    MoqVideoDecodedFrame as VideoDecodedFrame,
)
from moq_ffi import (
    MoqVideoEncoderInput as VideoEncoderInput,
)
from moq_ffi import (
    MoqVideoEncoderKind as VideoEncoderKind,
)
from moq_ffi import (
    MoqVideoEncoderOutput as VideoEncoderOutput,
)
from moq_ffi import (
    MoqVideoGop as VideoGop,
)
from moq_ffi import (
    MoqVideoPixelFormat as VideoPixelFormat,
)

from ._records import (
    Backoff,
    ClientTls,
    ConnectionStats,
    Datagram,
    Frame,
    Grant,
    QuicConfig,
    Subscription,
    TrackInfo,
    WebSocketConfig,
)

__all__ = [
    "AnnounceRoute",
    "Anchor",
    "AudioCodec",
    "AudioEncoderInput",
    "AudioPreset",
    "AudioSampleFormat",
    "Backoff",
    "ClientTls",
    "ConnectionStats",
    "ConnectionStatus",
    "Datagram",
    "ErrorScope",
    "Frame",
    "FetchGroupOptions",
    "Grant",
    "Order",
    "ProtocolError",
    "ProtocolKind",
    "QuicConfig",
    "Range",
    "Route",
    "Subscription",
    "TrackInfo",
    "VideoCodec",
    "VideoEncoderInput",
    "VideoEncoderKind",
    "VideoGop",
    "VideoEncoderOutput",
    "VideoDecodedFrame",
    "VideoPixelFormat",
    "WebSocketConfig",
]
